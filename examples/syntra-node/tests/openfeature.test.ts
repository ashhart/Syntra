/**
 * Tests for SyntraOpenFeatureProvider.
 *
 * fetch is mocked throughout; no real network calls are made.
 *
 * Apache-2.0
 */

import { jest, describe, it, expect, beforeEach, afterEach } from "@jest/globals";
import { OpenFeature } from "@openfeature/server-sdk";
import type { Logger } from "@openfeature/core";
import { SyntraOpenFeatureProvider } from "../src/openfeature.js";

function makeResponse(status: number, body: unknown = {}): Response {
  return {
    ok: status >= 200 && status < 300,
    status,
    json: async () => body,
    headers: new Headers(),
    redirected: false,
    statusText: String(status),
    type: "basic",
    url: "",
    clone: () => makeResponse(status, body),
    arrayBuffer: async () => new ArrayBuffer(0),
    blob: async () => new Blob([]),
    formData: async () => new FormData(),
    text: async () => JSON.stringify(body),
    body: null,
    bodyUsed: false,
  } as unknown as Response;
}

const logger = console as unknown as Logger;

function decideBody(optionIdx: number, decisionId = "dec_of_123") {
  return {
    decisionId,
    refused: false,
    decisions: [
      {
        node_id: 7,
        chosen_option: optionIdx,
        option: `option-${optionIdx}`,
        candidateId: "LinUcb",
      },
    ],
  };
}

function buildProvider(onFeedbackError?: (err: unknown) => void) {
  return new SyntraOpenFeatureProvider({
    baseUrl: "http://syntra.local:8787",
    adminKey: "test-key",
    defaultCapsulePath: "/tenants/t/jobs/j/capsules/c",
    timeoutMs: 500,
    onFeedbackError,
    flags: {
      "retry-policy": {
        variants: [
          { name: "none", value: "none" },
          { name: "single", value: "single" },
          { name: "triple", value: "triple" },
        ],
      },
      "object-policy": {
        variants: [
          { name: "cheap", value: { route: "cheap", maxConcurrent: 2 } },
          { name: "fast", value: { route: "fast", maxConcurrent: 8 } },
        ],
      },
    },
  });
}

describe("SyntraOpenFeatureProvider", () => {
  let mockFetch: jest.MockedFunction<typeof fetch>;

  beforeEach(() => {
    mockFetch = jest.fn<typeof fetch>();
    (global as Record<string, unknown>)["fetch"] = mockFetch;
  });

  afterEach(async () => {
    await OpenFeature.clearProviders();
    jest.restoreAllMocks();
  });

  it("resolves string flags through the standard OpenFeature client", async () => {
    mockFetch.mockResolvedValueOnce(makeResponse(200, decideBody(2, "dec_string")));

    await OpenFeature.setProviderAndWait(buildProvider());
    const client = OpenFeature.getClient("syntra-test");
    const details = await client.getStringDetails(
      "retry-policy",
      "single",
      {
        targetingKey: "account-42",
        features: {
          recent_failure_rate: 0.2,
          premium_tier: true,
        },
      },
    );

    expect(details.value).toBe("triple");
    expect(details.variant).toBe("triple");
    expect(details.reason).toBe("TARGETING_MATCH");
    expect(details.flagMetadata.syntraDecisionId).toBe("dec_string");
    expect(details.flagMetadata.syntraCandidateId).toBe("LinUcb");

    const [url, init] = mockFetch.mock.calls[0];
    expect(url).toBe("http://syntra.local:8787/tenants/t/jobs/j/capsules/c/decide");
    expect((init?.headers as Record<string, string>).Authorization).toBe("Bearer test-key");
    expect(JSON.parse(init?.body as string)).toEqual({
      features: {
        recent_failure_rate: 0.2,
        premium_tier: 1,
      },
    });
  });

  it("uses chosen_option directly for numeric flags without explicit variants", async () => {
    mockFetch.mockResolvedValueOnce(makeResponse(200, decideBody(5, "dec_number")));

    const provider = buildProvider();
    const details = await provider.resolveNumberEvaluation(
      "numeric-policy",
      0,
      { features: { load: 0.7 } },
      logger,
    );

    expect(details.value).toBe(5);
    expect(details.variant).toBe("5");
    expect(details.flagMetadata?.syntraChosenOption).toBe(5);
  });

  it("returns the OpenFeature default when Syntra is unreachable", async () => {
    mockFetch.mockRejectedValueOnce(new Error("ECONNREFUSED"));

    const provider = buildProvider();
    const details = await provider.resolveStringEvaluation(
      "retry-policy",
      "single",
      { targetingKey: "tenant-a" },
      logger,
    );

    expect(details.value).toBe("single");
    expect(details.variant).toBe("default");
    expect(details.reason).toBe("ERROR");
    expect(details.errorMessage).toContain("ECONNREFUSED");
  });

  it("returns object variants for object flag evaluations", async () => {
    mockFetch.mockResolvedValueOnce(makeResponse(200, decideBody(1, "dec_object")));

    const provider = buildProvider();
    const details = await provider.resolveObjectEvaluation(
      "object-policy",
      { route: "safe", maxConcurrent: 1 },
      { targetingKey: "account-99" },
      logger,
    );

    expect(details.value).toEqual({ route: "fast", maxConcurrent: 8 });
    expect(details.variant).toBe("fast");
  });

  it("posts feedback from OpenFeature tracking details", async () => {
    mockFetch.mockResolvedValueOnce(makeResponse(200, { ok: true }));
    const feedbackErrors: unknown[] = [];
    const provider = buildProvider((err) => feedbackErrors.push(err));

    provider.track(
      "syntra.feedback",
      {},
      {
        flagKey: "retry-policy",
        decisionId: "dec_track",
        decisionIndex: 1,
        reward: 0.9,
      },
    );

    await new Promise((r) => setTimeout(r, 20));

    expect(feedbackErrors).toHaveLength(0);
    expect(mockFetch).toHaveBeenCalledTimes(1);
    const [url, init] = mockFetch.mock.calls[0];
    expect(url).toBe("http://syntra.local:8787/tenants/t/jobs/j/capsules/c/feedback");
    expect(JSON.parse(init?.body as string)).toEqual({
      decisionId: "dec_track",
      decisionIndex: 1,
      reward: 0.9,
    });
  });

  it("reports invalid feedback tracking payloads through onFeedbackError", async () => {
    const feedbackErrors: unknown[] = [];
    const provider = buildProvider((err) => feedbackErrors.push(err));

    provider.track("syntra.feedback", {}, { flagKey: "retry-policy" });

    expect(feedbackErrors).toHaveLength(1);
    expect(String(feedbackErrors[0])).toContain("decisionId");
    expect(mockFetch).not.toHaveBeenCalled();
  });
});
