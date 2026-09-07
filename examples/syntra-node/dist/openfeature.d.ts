/**
 * SyntraOpenFeatureProvider
 *
 * OpenFeature server provider that resolves flags by calling Syntra /decide.
 * This lets Node services adopt Syntra through the standard OpenFeature SDK
 * while preserving Syntra's fail-safe default-value behavior.
 *
 * Apache-2.0
 */
import type { Provider } from "@openfeature/server-sdk";
import type { EvaluationContext, JsonValue, Logger, ResolutionDetails, TrackingEventDetails } from "@openfeature/core";
import type { FeedbackInput, SyntraClientOptions } from "./index.js";
/** One OpenFeature variant mapped from a Syntra chosen_option index. */
export interface SyntraFlagVariant<T extends JsonValue = JsonValue> {
    /** Value returned to OpenFeature callers when this option index wins. */
    value: T;
    /** Optional stable variant label. Defaults to the numeric option index. */
    name?: string;
}
/** Per-flag configuration for the Syntra OpenFeature provider. */
export interface SyntraOpenFeatureFlag<T extends JsonValue = JsonValue> {
    /** Override the provider default capsule path for this flag. */
    capsulePath?: string;
    /** Values indexed by Syntra's chosen_option. Required for string/object flags. */
    variants?: readonly SyntraFlagVariant<T>[];
    /** 0-based entry in decisions[] to read for multi-decision capsules. */
    decisionIndex?: number;
}
/** Provider configuration. */
export interface SyntraOpenFeatureProviderOptions extends Omit<SyntraClientOptions, "capsulePath"> {
    /** Capsule path used by flags that do not provide their own capsulePath. */
    defaultCapsulePath?: string;
    /** Per-flag variant and capsule mapping. */
    flags?: Record<string, SyntraOpenFeatureFlag>;
    /** OpenFeature tracking event name that should post /feedback. */
    feedbackEventName?: string;
    /** Optional observer for asynchronous feedback failures. */
    onFeedbackError?: (err: unknown) => void;
}
/**
 * OpenFeature provider backed by Syntra.
 *
 * Resolution calls:
 *   OpenFeature flagKey -> Syntra capsule -> /decide -> chosen_option -> variant value
 *
 * Feedback calls:
 *   client.track("syntra.feedback", ..., { flagKey, decisionId, reward })
 */
export declare class SyntraOpenFeatureProvider implements Provider {
    readonly metadata: {
        name: string;
    };
    readonly hooks: never[];
    private readonly baseOptions;
    private readonly defaultCapsulePath;
    private readonly flags;
    private readonly feedbackEventName;
    private readonly onFeedbackError;
    private readonly clients;
    constructor(options: SyntraOpenFeatureProviderOptions);
    resolveBooleanEvaluation(flagKey: string, defaultValue: boolean, context: EvaluationContext, _logger: Logger): Promise<ResolutionDetails<boolean>>;
    resolveStringEvaluation(flagKey: string, defaultValue: string, context: EvaluationContext, _logger: Logger): Promise<ResolutionDetails<string>>;
    resolveNumberEvaluation(flagKey: string, defaultValue: number, context: EvaluationContext, _logger: Logger): Promise<ResolutionDetails<number>>;
    resolveObjectEvaluation<T extends JsonValue>(flagKey: string, defaultValue: T, context: EvaluationContext, _logger: Logger): Promise<ResolutionDetails<T>>;
    /**
     * Posts feedback for a prior OpenFeature evaluation.
     *
     * This is useful when an application prefers an explicit promise instead of
     * OpenFeature's fire-and-forget track() path.
     */
    feedback(flagKey: string, input: FeedbackInput): Promise<void>;
    /**
     * OpenFeature tracking hook. The SDK calls this when application code invokes
     * client.track(). Only the configured feedback event name is handled.
     */
    track(trackingEventName: string, context: EvaluationContext, trackingEventDetails?: TrackingEventDetails): void;
    private resolveFlag;
    private capsulePathFor;
    private clientFor;
}
//# sourceMappingURL=openfeature.d.ts.map