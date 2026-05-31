/**
 * SyntraOpenFeatureProvider
 *
 * OpenFeature server provider that resolves flags by calling Syntra /decide.
 * This lets Node services adopt Syntra through the standard OpenFeature SDK
 * while preserving Syntra's fail-safe default-value behavior.
 *
 * Apache-2.0
 */
import { SyntraClient } from "./index.js";
/**
 * OpenFeature provider backed by Syntra.
 *
 * Resolution calls:
 *   OpenFeature flagKey -> Syntra capsule -> /decide -> chosen_option -> variant value
 *
 * Feedback calls:
 *   client.track("syntra.feedback", ..., { flagKey, decisionId, reward })
 */
export class SyntraOpenFeatureProvider {
    metadata = { name: "SyntraOpenFeatureProvider" };
    hooks = [];
    baseOptions;
    defaultCapsulePath;
    flags;
    feedbackEventName;
    onFeedbackError;
    clients = new Map();
    constructor(options) {
        this.baseOptions = {
            baseUrl: options.baseUrl,
            adminKey: options.adminKey,
            timeoutMs: options.timeoutMs,
        };
        this.defaultCapsulePath = options.defaultCapsulePath;
        this.flags = options.flags ?? {};
        this.feedbackEventName = options.feedbackEventName ?? "syntra.feedback";
        this.onFeedbackError = options.onFeedbackError;
    }
    async resolveBooleanEvaluation(flagKey, defaultValue, context, _logger) {
        return this.resolveFlag(flagKey, defaultValue, context, isBooleanValue);
    }
    async resolveStringEvaluation(flagKey, defaultValue, context, _logger) {
        return this.resolveFlag(flagKey, defaultValue, context, isStringValue);
    }
    async resolveNumberEvaluation(flagKey, defaultValue, context, _logger) {
        return this.resolveFlag(flagKey, defaultValue, context, isNumberValue);
    }
    async resolveObjectEvaluation(flagKey, defaultValue, context, _logger) {
        const isExpectedObject = (value) => isJsonValue(value);
        return this.resolveFlag(flagKey, defaultValue, context, isExpectedObject);
    }
    /**
     * Posts feedback for a prior OpenFeature evaluation.
     *
     * This is useful when an application prefers an explicit promise instead of
     * OpenFeature's fire-and-forget track() path.
     */
    async feedback(flagKey, input) {
        const capsulePath = this.capsulePathFor(flagKey);
        await this.clientFor(capsulePath).feedback(input);
    }
    /**
     * OpenFeature tracking hook. The SDK calls this when application code invokes
     * client.track(). Only the configured feedback event name is handled.
     */
    track(trackingEventName, context, trackingEventDetails = {}) {
        if (trackingEventName !== this.feedbackEventName) {
            return;
        }
        try {
            const input = feedbackFromTracking(context, trackingEventDetails);
            const flagKey = stringFromTracking("flagKey", trackingEventDetails);
            const capsulePathOverride = stringFromTracking("capsulePath", trackingEventDetails);
            const capsulePath = capsulePathOverride ?? (flagKey !== undefined ? this.capsulePathFor(flagKey) : this.defaultCapsulePath);
            if (capsulePath === undefined) {
                throw new Error("Syntra feedback tracking requires flagKey or capsulePath");
            }
            this.clientFor(capsulePath).feedback(input).catch((err) => {
                this.onFeedbackError?.(err);
            });
        }
        catch (err) {
            this.onFeedbackError?.(err);
        }
    }
    async resolveFlag(flagKey, defaultValue, context, isExpected) {
        const capsulePath = this.capsulePathFor(flagKey);
        const flag = this.flags[flagKey] ?? {};
        const decisionIndex = flag.decisionIndex ?? 0;
        try {
            const data = await this.clientFor(capsulePath).decide(decideInputFromContext(context));
            if (data.refused) {
                return fallbackDetails(defaultValue, "DEFAULT", {
                    syntraCapsulePath: capsulePath,
                    syntraDecisionId: data.decisionId ?? "",
                    syntraDecisionIndex: decisionIndex,
                    syntraRefused: true,
                });
            }
            const decision = data.decisions?.[decisionIndex];
            if (decision === undefined || !Number.isInteger(decision.chosen_option)) {
                return fallbackDetails(defaultValue, "ERROR", {
                    syntraCapsulePath: capsulePath,
                    syntraDecisionId: data.decisionId ?? "",
                    syntraDecisionIndex: decisionIndex,
                    syntraError: "missing decision entry",
                });
            }
            const chosenOption = decision.chosen_option;
            const variant = flag.variants?.[chosenOption];
            const resolvedValue = variant !== undefined
                ? variant.value
                : valueFromOption(defaultValue, chosenOption);
            if (!isExpected(resolvedValue)) {
                return fallbackDetails(defaultValue, "ERROR", {
                    syntraCapsulePath: capsulePath,
                    syntraDecisionId: data.decisionId ?? "",
                    syntraDecisionIndex: decisionIndex,
                    syntraChosenOption: chosenOption,
                    syntraError: "variant type mismatch",
                });
            }
            return {
                value: resolvedValue,
                variant: variant?.name ?? String(chosenOption),
                reason: "TARGETING_MATCH",
                flagMetadata: metadataForDecision(capsulePath, data.decisionId, decisionIndex, decision),
            };
        }
        catch (err) {
            return fallbackDetails(defaultValue, "ERROR", {
                syntraCapsulePath: capsulePath,
                syntraDecisionIndex: decisionIndex,
                syntraError: errorMessage(err),
            });
        }
    }
    capsulePathFor(flagKey) {
        const capsulePath = this.flags[flagKey]?.capsulePath ?? this.defaultCapsulePath;
        if (capsulePath === undefined || capsulePath.trim() === "") {
            throw new Error(`No Syntra capsulePath configured for OpenFeature flag '${flagKey}'`);
        }
        return capsulePath;
    }
    clientFor(capsulePath) {
        const normalizedPath = capsulePath.replace(/\/$/, "");
        const existing = this.clients.get(normalizedPath);
        if (existing !== undefined) {
            return existing;
        }
        const client = new SyntraClient({
            ...this.baseOptions,
            capsulePath: normalizedPath,
        });
        this.clients.set(normalizedPath, client);
        return client;
    }
}
function decideInputFromContext(context) {
    const features = featuresFromContext(context);
    if (features !== undefined) {
        return { features };
    }
    if (typeof context.targetingKey === "string" && context.targetingKey.length > 0) {
        return { contextKey: context.targetingKey };
    }
    return { contextKey: "default" };
}
function featuresFromContext(context) {
    const explicitFeatures = numberRecordFromUnknown(context.features);
    if (explicitFeatures !== undefined) {
        return explicitFeatures;
    }
    const features = {};
    for (const [key, value] of Object.entries(context)) {
        if (key === "targetingKey") {
            continue;
        }
        const numeric = numericFeatureValue(value);
        if (numeric !== undefined) {
            features[key] = numeric;
        }
    }
    return Object.keys(features).length > 0 ? features : undefined;
}
function numberRecordFromUnknown(value) {
    if (!isPlainRecord(value)) {
        return undefined;
    }
    const out = {};
    for (const [key, raw] of Object.entries(value)) {
        const numeric = numericFeatureValue(raw);
        if (numeric !== undefined) {
            out[key] = numeric;
        }
    }
    return Object.keys(out).length > 0 ? out : undefined;
}
function numericFeatureValue(value) {
    if (typeof value === "number" && Number.isFinite(value)) {
        return value;
    }
    if (typeof value === "boolean") {
        return value ? 1 : 0;
    }
    return undefined;
}
function valueFromOption(defaultValue, option) {
    if (typeof defaultValue === "number") {
        return option;
    }
    if (typeof defaultValue === "boolean") {
        return option !== 0;
    }
    return undefined;
}
function metadataForDecision(capsulePath, decisionId, decisionIndex, decision) {
    const metadata = {
        syntraCapsulePath: capsulePath,
        syntraDecisionId: decisionId ?? "",
        syntraDecisionIndex: decisionIndex,
        syntraChosenOption: decision.chosen_option,
    };
    if (typeof decision.node_id === "number") {
        metadata.syntraNodeId = decision.node_id;
    }
    if (typeof decision.option === "string") {
        metadata.syntraOption = decision.option;
    }
    if (typeof decision.candidateId === "string") {
        metadata.syntraCandidateId = decision.candidateId;
    }
    return metadata;
}
function fallbackDetails(defaultValue, reason, flagMetadata) {
    return {
        value: defaultValue,
        reason,
        variant: "default",
        flagMetadata,
        errorMessage: typeof flagMetadata.syntraError === "string" ? flagMetadata.syntraError : undefined,
    };
}
function feedbackFromTracking(context, details) {
    const decisionId = stringFromTracking("decisionId", details) ??
        (typeof context.syntraDecisionId === "string" ? context.syntraDecisionId : undefined);
    if (decisionId === undefined || decisionId.length === 0) {
        throw new Error("Syntra feedback tracking requires decisionId");
    }
    const decisionIndex = numberFromTracking("decisionIndex", details) ??
        (typeof context.syntraDecisionIndex === "number" ? context.syntraDecisionIndex : undefined);
    const reward = numberFromTracking("reward", details);
    if (reward !== undefined) {
        return { decisionId, decisionIndex, reward };
    }
    const rewardComponents = numberRecordFromUnknown(details.rewardComponents);
    if (rewardComponents !== undefined) {
        return { decisionId, decisionIndex, rewardComponents };
    }
    throw new Error("Syntra feedback tracking requires reward or rewardComponents");
}
function stringFromTracking(key, details) {
    const value = details[key];
    return typeof value === "string" ? value : undefined;
}
function numberFromTracking(key, details) {
    const value = details[key];
    return typeof value === "number" && Number.isFinite(value) ? value : undefined;
}
function isBooleanValue(value) {
    return typeof value === "boolean";
}
function isStringValue(value) {
    return typeof value === "string";
}
function isNumberValue(value) {
    return typeof value === "number" && Number.isFinite(value);
}
function isJsonValue(value) {
    if (value === null) {
        return true;
    }
    if (["string", "number", "boolean"].includes(typeof value)) {
        return typeof value !== "number" || Number.isFinite(value);
    }
    if (Array.isArray(value)) {
        return value.every(isJsonValue);
    }
    if (isPlainRecord(value)) {
        return Object.values(value).every(isJsonValue);
    }
    return false;
}
function isPlainRecord(value) {
    return typeof value === "object" && value !== null && !Array.isArray(value);
}
function errorMessage(err) {
    return err instanceof Error ? err.message : String(err);
}
//# sourceMappingURL=openfeature.js.map