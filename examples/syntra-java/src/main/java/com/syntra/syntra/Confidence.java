// Copyright 2024 Syntra. Apache-2.0.
package com.syntra.syntra;

import java.util.OptionalDouble;

/**
 * Confidence block returned alongside a {@code /decide} response when the
 * capsule has refusal enabled.
 *
 * <p>All fields are optional because older capsule versions may omit them.
 */
public record Confidence(
    OptionalDouble oodScore,
    OptionalDouble intervalWidth,
    boolean refused
) {}
