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
import type {
  EvaluationContext,
  FlagMetadata,
  JsonValue,
  Logger,
  ResolutionDetails,
  TrackingEventDetails,
} from "@openfeature/core";
import { SyntraClient } from "./index.js";
import type { DecideInput, FeedbackInput, SyntraClientOptions } from "./index.js";

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
export interface SyntraOpenFeatureProviderOptions
  extends Omit<SyntraClientOptions, "capsulePath"> {
  /** Capsule path used by flags that do not provide their own capsulePath. */
  defaultCapsulePath?: string;
  /** Per-flag variant and capsule mapping. */
  flags?: Record<string, SyntraOpenFeatureFlag>;
  /** OpenFeature tracking event name that should post /feedback. */
  feedbackEventName?: string;
  /** Optional observer for asynchronous feedback failures. */
  onFeedbackError?: (err: unknown) => void;
}

type ExpectedValue<T extends JsonValue> = (value: unknown) => value is T;

/**
 * OpenFeature provider backed by Syntra.
 *
 * Resolution calls:
 *   OpenFeature flagKey -> Syntra capsule -> /decide -> chosen_option -> variant value
 *
 * Feedback calls:
 *   client.track("syntra.feedback", ..., { flagKey, decisionId, reward })
 */
export class SyntraOpenFeatureProvider implements Provider {
  readonly metadata = { name: "SyntraOpenFeatureProvider" };
  readonly hooks = [];

  private readonly baseOptions: Omit<SyntraClientOptions, "capsulePath">;
  private readonly defaultCapsulePath: string | undefined;
  private readonly flags: Record<string, SyntraOpenFeatureFlag>;
  private readonly feedbackEventName: string;
  private readonly onFeedbackError: ((err: unknown) => void) | undefined;
  private readonly clients = new Map<string, SyntraClient>();

  constructor(options: SyntraOpenFeatureProviderOptions) {
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

  async resolveBooleanEvaluation(
    flagKey: string,
    defaultValue: boolean,
    context: EvaluationContext,
    _logger: Logger,
  ): Promise<ResolutionDetails<boolean>> {
    return this.resolveFlag(flagKey, defaultValue, context, isBooleanValue);
  }

  async resolveStringEvaluation(
    flagKey: string,
    defaultValue: string,
    context: EvaluationContext,
    _logger: Logger,
  ): Promise<ResolutionDetails<string>> {
    return this.resolveFlag(flagKey, defaultValue, context, isStringValue);
  }

  async resolveNumberEvaluation(
    flagKey: string,
    defaultValue: number,
    context: EvaluationContext,
    _logger: Logger,
  ): Promise<ResolutionDetails<number>> {
    return this.resolveFlag(flagKey, defaultValue, context, isNumberValue);
  }

  async resolveObjectEvaluation<T extends JsonValue>(
    flagKey: string,
    defaultValue: T,
    context: EvaluationContext,
    _logger: Logger,
  ): Promise<ResolutionDetails<T>> {
    const isExpectedObject = (value: unknown): value is T => isJsonValue(value);
    return this.resolveFlag(flagKey, defaultValue, context, isExpectedObject);
  }

  /**
   * Posts feedback for a prior OpenFeature evaluation.
   *
   * This is useful when an application prefers an explicit promise instead of
   * OpenFeature's fire-and-forget track() path.
   */
  async feedback(flagKey: string, input: FeedbackInput): Promise<void> {
    const capsulePath = this.capsulePathFor(flagKey);
    await this.clientFor(capsulePath).feedback(input);
  }

  /**
   * OpenFeature tracking hook. The SDK calls this when application code invokes
   * client.track(). Only the configured feedback event name is handled.
   */
  track(
    trackingEventName: string,
    context: EvaluationContext,
    trackingEventDetails: TrackingEventDetails = {},
  ): void {
    if (trackingEventName !== this.feedbackEventName) {
      return;
    }

    try {
      const input = feedbackFromTracking(context, trackingEventDetails);
      const flagKey = stringFromTracking("flagKey", trackingEventDetails);
      const capsulePathOverride = stringFromTracking("capsulePath", trackingEventDetails);
      const capsulePath =
        capsulePathOverride ?? (flagKey !== undefined ? this.capsulePathFor(flagKey) : this.defaultCapsulePath);

      if (capsulePath === undefined) {
        throw new Error("Syntra feedback tracking requires flagKey or capsulePath");
      }

      this.clientFor(capsulePath).feedback(input).catch((err: unknown) => {
        this.onFeedbackError?.(err);
      });
    } catch (err) {
      this.onFeedbackError?.(err);
    }
  }

  private async resolveFlag<T extends JsonValue>(
    flagKey: string,
    defaultValue: T,
    context: EvaluationContext,
    isExpected: ExpectedValue<T>,
  ): Promise<ResolutionDetails<T>> {
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
      const resolvedValue =
        variant !== undefined
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
    } catch (err) {
      return fallbackDetails(defaultValue, "ERROR", {
        syntraCapsulePath: capsulePath,
        syntraDecisionIndex: decisionIndex,
        syntraError: errorMessage(err),
      });
    }
  }

  private capsulePathFor(flagKey: string): string {
    const capsulePath = this.flags[flagKey]?.capsulePath ?? this.defaultCapsulePath;
    if (capsulePath === undefined || capsulePath.trim() === "") {
      throw new Error(`No Syntra capsulePath configured for OpenFeature flag '${flagKey}'`);
    }
    return capsulePath;
  }

  private clientFor(capsulePath: string): SyntraClient {
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

function decideInputFromContext(context: EvaluationContext): DecideInput {
  const features = featuresFromContext(context);
  if (features !== undefined) {
    return { features };
  }

  if (typeof context.targetingKey === "string" && context.targetingKey.length > 0) {
    return { contextKey: context.targetingKey };
  }

  return { contextKey: "default" };
}

function featuresFromContext(context: EvaluationContext): Record<string, number> | undefined {
  const explicitFeatures = numberRecordFromUnknown(context.features);
  if (explicitFeatures !== undefined) {
    return explicitFeatures;
  }

  const features: Record<string, number> = {};
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

function numberRecordFromUnknown(value: unknown): Record<string, number> | undefined {
  if (!isPlainRecord(value)) {
    return undefined;
  }

  const out: Record<string, number> = {};
  for (const [key, raw] of Object.entries(value)) {
    const numeric = numericFeatureValue(raw);
    if (numeric !== undefined) {
      out[key] = numeric;
    }
  }

  return Object.keys(out).length > 0 ? out : undefined;
}

function numericFeatureValue(value: unknown): number | undefined {
  if (typeof value === "number" && Number.isFinite(value)) {
    return value;
  }
  if (typeof value === "boolean") {
    return value ? 1 : 0;
  }
  return undefined;
}

function valueFromOption(defaultValue: JsonValue, option: number): JsonValue | undefined {
  if (typeof defaultValue === "number") {
    return option;
  }
  if (typeof defaultValue === "boolean") {
    return option !== 0;
  }
  return undefined;
}

function metadataForDecision(
  capsulePath: string,
  decisionId: string | undefined,
  decisionIndex: number,
  decision: Record<string, unknown>,
): FlagMetadata {
  const metadata: FlagMetadata = {
    syntraCapsulePath: capsulePath,
    syntraDecisionId: decisionId ?? "",
    syntraDecisionIndex: decisionIndex,
    syntraChosenOption: decision.chosen_option as number,
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

function fallbackDetails<T extends JsonValue>(
  defaultValue: T,
  reason: "DEFAULT" | "ERROR",
  flagMetadata: FlagMetadata,
): ResolutionDetails<T> {
  return {
    value: defaultValue,
    reason,
    variant: "default",
    flagMetadata,
    errorMessage: typeof flagMetadata.syntraError === "string" ? flagMetadata.syntraError : undefined,
  };
}

function feedbackFromTracking(
  context: EvaluationContext,
  details: TrackingEventDetails,
): FeedbackInput {
  const decisionId =
    stringFromTracking("decisionId", details) ??
    (typeof context.syntraDecisionId === "string" ? context.syntraDecisionId : undefined);
  if (decisionId === undefined || decisionId.length === 0) {
    throw new Error("Syntra feedback tracking requires decisionId");
  }

  const decisionIndex =
    numberFromTracking("decisionIndex", details) ??
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

function stringFromTracking(
  key: string,
  details: TrackingEventDetails,
): string | undefined {
  const value = details[key];
  return typeof value === "string" ? value : undefined;
}

function numberFromTracking(
  key: string,
  details: TrackingEventDetails,
): number | undefined {
  const value = details[key];
  return typeof value === "number" && Number.isFinite(value) ? value : undefined;
}

function isBooleanValue(value: unknown): value is boolean {
  return typeof value === "boolean";
}

function isStringValue(value: unknown): value is string {
  return typeof value === "string";
}

function isNumberValue(value: unknown): value is number {
  return typeof value === "number" && Number.isFinite(value);
}

function isJsonValue(value: unknown): value is JsonValue {
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

function isPlainRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function errorMessage(err: unknown): string {
  return err instanceof Error ? err.message : String(err);
}
