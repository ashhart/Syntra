# Moving from Azure AI Personalizer

Azure AI Personalizer retires on 1 October 2026. Syntra serves
Personalizer's v1.0 rank, reward, activate and service-configuration API
on top of a capsule, so a client that uses those calls moves by changing
its endpoint and key. Its decisions then land in Syntra's log with their
propensities, where you can evaluate and promote changes
([quickstart](../quickstart.md#6-evaluate-a-change-before-it-serves)).
Before any traffic moves, you can import the history Personalizer logged
and evaluate on it.

The examples use the `syntra` binary, a store at `./syntra-store`, a server
at `$S` (for example `http://127.0.0.1:8787`, or your TLS endpoint) and its
admin key in `$KEY`.

## 1. Bring the history (optional)

Personalizer, like the Vowpal Wabbit decision service, logs events as
DSJSON, one JSON event per line with the context, the actions, the
probabilities they were ranked with and the reward. `syntra import dsjson`
loads such a file into a capsule's log, so `syntra evaluate` can score
policies on months of real traffic before the new endpoint serves any.
Import while no server runs on the store (the command refuses a live store
unless `--force`), for example before you start Syntra for the first time:

```bash
syntra import dsjson --store ./syntra-store --capsule acme/prod/news personalizer-log.json
# {"alreadyPresent":0,"decisions":2000,"errors":[],"lines":2000,"rewards":2000,
#  "skipped":{"deferredNotActivated":0,"invalid":0,"multiSlot":0}}
syntra evaluate --store ./syntra-store --capsule acme/prod/news --policy greedy --format markdown
```

Each event becomes a decision: `EventId` is its id, `Timestamp` its time,
the `c` namespaces (keys not starting with `_`) its context, `c._multi`
its actions (the id from `_tag`), `a` and `p` its eligible actions and
probabilities, and `-_label_cost` (or the sum of its `o` outcomes) its
reward. The importer skips (and counts) deferred events that were never
activated and multi-slot events. Importing the same file again skips what
is already there. The importer creates the capsule with the default spec
if it does not exist; the next step adjusts it.

Imported rewards are for evaluation only. The model does not learn from
them unless you pass `--learn`, in which case the capsule replays them the
next time it loads, a warm start. Evaluate before you decide to learn from
them. The lift of `greedy` over the logged policy shows what a model
trained on that history would have earned.

## 2. Start Syntra and set up the capsule

One Personalizer resource (one loop) becomes one capsule. Start the server
on the store, then create the capsule (or adjust the one the import
created). Actions arrive with every rank call, so the spec declares none:

```bash
syntra serve --store ./syntra-store --admin-key "$KEY" &
sleep 1
C=$S/v1/tenants/acme/jobs/prod/capsules/news
curl -s -X PUT $C/spec -H "Authorization: Bearer $KEY" \
  -d '{"actions": [], "exploration": {"kind": "epsilonGreedy", "epsilon": 0.2}}'
```

`epsilonGreedy` with `epsilon` is what Personalizer's exploration
percentage means. Syntra's default, SquareCB, instead gives each action a
probability that falls with the gap between its predicted reward and the
best one's, so it explores less as the model learns. Either way, a floor
keeps every action's probability at or above `exploration.floor / K`.

You can also set the same things in Personalizer's terms. Changing the
configuration needs a `tenant_admin` or admin key, on the capsule's path:

```bash
curl -s -X PUT $C/personalizer/v1.0/configurations/service -H "Authorization: Bearer $KEY" \
  -d '{"rewardWaitTime": "PT10M", "defaultReward": 0, "rewardAggregation": "earliest",
       "explorationPercentage": 0.2, "learningMode": "Online"}'
# {"defaultReward":0.0,"explorationPercentage":0.2,"learningMode":"Online","logRetentionDays":-1,
#  "modelExportFrequency":"PT1S","rewardAggregation":"earliest","rewardWaitTime":"PT10M"}
```

## 3. Issue the key your client will use

A `read` token scoped to the capsule is what replaces the Personalizer
key. The server binds it to that capsule, so the client needs no tenant
or capsule in its URLs:

```bash
TOKEN=$(curl -s -X POST $S/v1/admin/tokens -H "Authorization: Bearer $KEY" \
  -d '{"scope": {"kind": "read", "tenant": "acme", "job": "prod", "capsule": "news"}, "label": "personalizer"}' \
  | python3 -c 'import sys, json; print(json.load(sys.stdin)["token"])')
```

## 4. Point the client at Syntra

Set the client's endpoint to the Syntra server and its key to `$TOKEN`.
Clients call `<endpoint>/personalizer/v1.0/...` and send the key as
`Ocp-Apim-Subscription-Key` (or `Authorization: Bearer`):

```bash
curl -s -X POST $S/personalizer/v1.0/rank -H "Ocp-Apim-Subscription-Key: $TOKEN" \
  -d '{"contextFeatures": [{"user": {"tier": "pro"}}],
       "actions": [{"id": "sports", "features": [{"topic": "sports"}]}, {"id": "news", "features": [{"topic": "news"}]}],
       "eventId": "75269AD0-BFEE-4598-8196-C57383D38E10"}'
# {"eventId":"75269AD0-BFEE-4598-8196-C57383D38E10","ranking":[{"id":"sports","probability":0.88},
#  {"id":"news","probability":0.12}],"rewardActionId":"sports"}
curl -s -X POST $S/personalizer/v1.0/events/75269AD0-BFEE-4598-8196-C57383D38E10/reward \
  -H "Ocp-Apim-Subscription-Key: $TOKEN" -d '{"value": 1.0}' -w '%{http_code}\n'
# 204
```

| Personalizer | Syntra |
|---|---|
| `POST rank` | A decision: `contextFeatures` merged into one context object (each object's keys act as namespaces), each action's `features` merged the same way, `excludedActions` removed. Answers 201 with the chosen action first, then the other eligible actions by probability, then excluded ones at 0. |
| `eventId` | The decision id: 1-128 characters from `A-Z a-z 0-9 _ . : -`, generated when absent. Resending the same request returns the same answer; a different request with a used id is a 409. |
| `deferActivation: true` | The decision is held, neither logged nor learned from, until `events/{eventId}/activate`; rewards that arrive first are held with it. Held events survive a graceful restart (not a crash) and are dropped after 24 hours if never activated. |
| `POST events/{eventId}/reward` | A reward for the decision (204). |
| `POST events/{eventId}/activate` | 204, also for an event that is already active; 404 for an unknown one. |
| `GET`/`PUT configurations/service` | `rewardWaitTime` is `reward.waitSeconds`, `defaultReward` is `reward.default`, `rewardAggregation` (`earliest` or `sum`) is `rewards` (`first` or `sum`), `explorationPercentage` sets epsilon-greedy exploration, and `learningMode` `Online`, `Apprentice` or `Frozen` sets the mode `learner`, `baselineExplore` or `frozen`. Other fields are accepted and ignored; `modelExportFrequency` reads `PT1S` and `logRetentionDays` `-1`. |
| Errors | Personalizer's shape: `{"error": {"code": "BadArgument", "message": "..."}}`, with codes `BadArgument`, `Unauthorized`, `Forbidden`, `ResourceNotFound`, `Conflict`, `TooManyRequests`, `ServiceUnavailable` and `InternalServerError`. |

Differences to know:

- **Apprentice mode** is `baselineExplore` with the first action of each
  rank call as the baseline. The baseline gets `1 - baselineEpsilon`
  plus an even share of `baselineEpsilon` (0.95 with two actions at the
  default 0.1); `explorationPercentage` does not apply in this mode. Set
  `baselineEpsilon` with `POST .../mode`.
- **Missing rewards.** With `defaultReward` set, a decision that gets no
  reward within `rewardWaitTime` receives the default, as in
  Personalizer. Without it, it stays unrewarded and off-policy evaluation
  skips it.
- **Not supported.** Multi-slot ranking, the `LoggingOnly` learning mode,
  and Personalizer's policy, model, log and evaluation endpoints, which
  answer 404. Evaluation is Syntra's own: `syntra evaluate` and
  `POST .../evaluate`.
- **Learning** is Syntra's own, with one online least-squares model over
  hashed features of the context crossed with each action's features,
  trained on every reward. The importer takes Personalizer's logs, not its
  models.

## 5. Check the move

- `GET $C/decisions` lists the ranked events (imported and live) with
  their probabilities; `GET $C/decisions/{eventId}` shows one with its
  rewards.
- `syntra_requests_total{route="personalizer.rank"}` and
  `{route="personalizer.reward"}` in `/metrics` count the traffic.
- `POST $C/evaluate` with an admin or `tenant_admin` key scores policies
  on the new log; `POST $C/promote` changes the spec only if it passes
  your gates.
