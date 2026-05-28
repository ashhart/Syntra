// Copyright 2024 Ash Hart. Apache-2.0.
package com.ashhart.syntra;

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
