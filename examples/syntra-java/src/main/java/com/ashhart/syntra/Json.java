// Copyright 2024 Ash Hart. Apache-2.0.
package com.ashhart.syntra;

import java.util.ArrayList;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;
import java.util.OptionalDouble;

/**
 * Minimal hand-rolled JSON encoder/decoder limited to the shapes exchanged with
 * the Syntra appliance.
 *
 * <h2>Why not Jackson / Gson?</h2>
 * <p>This library aims for zero production dependencies.  The Syntra wire
 * protocol uses only two request shapes ({@code /decide} and {@code /feedback})
 * and one response shape ({@code /decide}).  A full-featured JSON library would
 * be overkill and would force consumers to manage a transitive dependency they
 * may not want.  This file is intentionally small (~250 lines) and covers
 * exactly the shapes we need.
 *
 * <h2>Limitations</h2>
 * <ul>
 *   <li>Encoding: supports {@link Map}, {@link List}, {@link String},
 *       {@link Number}, {@link Boolean}, and {@code null}.</li>
 *   <li>Decoding: returns a loosely typed tree of {@code Map<String,Object>},
 *       {@code List<Object>}, {@code String}, {@code Double}, and
 *       {@code Boolean}.</li>
 *   <li>No streaming, no schema validation, no Unicode escape handling beyond
 *       ASCII-safe strings produced by the appliance.</li>
 * </ul>
 */
final class Json {

    private Json() {}

    // -----------------------------------------------------------------------
    // Encoding
    // -----------------------------------------------------------------------

    /** Encodes a value to a JSON string.  Supports Map, List, String, Number, Boolean, null. */
    static String encode(Object value) {
        final StringBuilder sb = new StringBuilder();
        encodeValue(value, sb);
        return sb.toString();
    }

    private static void encodeValue(final Object value, final StringBuilder sb) {
        if (value == null) {
            sb.append("null");
        } else if (value instanceof String s) {
            sb.append('"');
            for (final char c : s.toCharArray()) {
                switch (c) {
                    case '"'  -> sb.append("\\\"");
                    case '\\' -> sb.append("\\\\");
                    case '\n' -> sb.append("\\n");
                    case '\r' -> sb.append("\\r");
                    case '\t' -> sb.append("\\t");
                    default   -> sb.append(c);
                }
            }
            sb.append('"');
        } else if (value instanceof Boolean b) {
            sb.append(b ? "true" : "false");
        } else if (value instanceof Number n) {
            sb.append(n);
        } else if (value instanceof Map<?, ?> map) {
            sb.append('{');
            boolean first = true;
            for (final Map.Entry<?, ?> entry : map.entrySet()) {
                if (!first) sb.append(',');
                first = false;
                encodeValue(entry.getKey().toString(), sb);
                sb.append(':');
                encodeValue(entry.getValue(), sb);
            }
            sb.append('}');
        } else if (value instanceof List<?> list) {
            sb.append('[');
            boolean first = true;
            for (final Object item : list) {
                if (!first) sb.append(',');
                first = false;
                encodeValue(item, sb);
            }
            sb.append(']');
        } else {
            // Fallback: toString, quoted
            encodeValue(value.toString(), sb);
        }
    }

    // -----------------------------------------------------------------------
    // Decoding
    // -----------------------------------------------------------------------

    /**
     * Parses a JSON string into a loosely typed tree.
     * Returns one of: {@code Map<String,Object>}, {@code List<Object>},
     * {@code String}, {@code Double}, {@code Boolean}, or {@code null}.
     */
    static Object decode(final String json) {
        final Parser p = new Parser(json.trim());
        return p.parseValue();
    }

    /** Convenience: decode and cast to {@code Map<String,Object>}. */
    @SuppressWarnings("unchecked")
    static Map<String, Object> decodeObject(final String json) {
        final Object v = decode(json);
        if (v instanceof Map<?, ?> m) return (Map<String, Object>) m;
        throw new IllegalArgumentException("Expected JSON object, got: " + (v == null ? "null" : v.getClass().getSimpleName()));
    }

    // -----------------------------------------------------------------------
    // Convenience accessors for decoded trees
    // -----------------------------------------------------------------------

    @SuppressWarnings("unchecked")
    static Map<String, Object> asObject(final Object v) {
        return (Map<String, Object>) v;
    }

    @SuppressWarnings("unchecked")
    static List<Object> asList(final Object v) {
        return (List<Object>) v;
    }

    static String asString(final Object v) {
        return v == null ? null : v.toString();
    }

    static boolean asBoolean(final Object v, final boolean defaultValue) {
        if (v instanceof Boolean b) return b;
        return defaultValue;
    }

    static double asDouble(final Object v, final double defaultValue) {
        if (v instanceof Number n) return n.doubleValue();
        return defaultValue;
    }

    static int asInt(final Object v, final int defaultValue) {
        if (v instanceof Number n) return n.intValue();
        return defaultValue;
    }

    static OptionalDouble asOptionalDouble(final Object v) {
        if (v instanceof Number n) return OptionalDouble.of(n.doubleValue());
        return OptionalDouble.empty();
    }

    // -----------------------------------------------------------------------
    // Parser
    // -----------------------------------------------------------------------

    private static final class Parser {
        private final String src;
        private int pos;

        Parser(final String src) {
            this.src = src;
            this.pos = 0;
        }

        Object parseValue() {
            skipWhitespace();
            if (pos >= src.length()) throw parseError("Unexpected end of input");
            final char c = src.charAt(pos);
            return switch (c) {
                case '"'  -> parseString();
                case '{'  -> parseObject();
                case '['  -> parseArray();
                case 't'  -> parseLiteral("true",  Boolean.TRUE);
                case 'f'  -> parseLiteral("false", Boolean.FALSE);
                case 'n'  -> parseLiteral("null",  null);
                default   -> parseNumber();
            };
        }

        private String parseString() {
            expect('"');
            final StringBuilder sb = new StringBuilder();
            while (pos < src.length()) {
                final char c = src.charAt(pos++);
                if (c == '"') return sb.toString();
                if (c == '\\') {
                    if (pos >= src.length()) break;
                    final char esc = src.charAt(pos++);
                    switch (esc) {
                        case '"'  -> sb.append('"');
                        case '\\' -> sb.append('\\');
                        case '/'  -> sb.append('/');
                        case 'n'  -> sb.append('\n');
                        case 'r'  -> sb.append('\r');
                        case 't'  -> sb.append('\t');
                        case 'b'  -> sb.append('\b');
                        case 'f'  -> sb.append('\f');
                        case 'u'  -> {
                            final String hex = src.substring(pos, Math.min(pos + 4, src.length()));
                            sb.append((char) Integer.parseInt(hex, 16));
                            pos += 4;
                        }
                        default -> sb.append(esc);
                    }
                } else {
                    sb.append(c);
                }
            }
            throw parseError("Unterminated string");
        }

        private Map<String, Object> parseObject() {
            expect('{');
            skipWhitespace();
            final Map<String, Object> map = new LinkedHashMap<>();
            if (peek() == '}') { pos++; return map; }
            while (true) {
                skipWhitespace();
                final String key = parseString();
                skipWhitespace();
                expect(':');
                skipWhitespace();
                final Object val = parseValue();
                map.put(key, val);
                skipWhitespace();
                final char next = src.charAt(pos);
                if (next == '}') { pos++; return map; }
                if (next == ',') { pos++; } else throw parseError("Expected ',' or '}'");
            }
        }

        private List<Object> parseArray() {
            expect('[');
            skipWhitespace();
            final List<Object> list = new ArrayList<>();
            if (peek() == ']') { pos++; return list; }
            while (true) {
                skipWhitespace();
                list.add(parseValue());
                skipWhitespace();
                final char next = src.charAt(pos);
                if (next == ']') { pos++; return list; }
                if (next == ',') { pos++; } else throw parseError("Expected ',' or ']'");
            }
        }

        private Object parseLiteral(final String text, final Object value) {
            if (src.startsWith(text, pos)) {
                pos += text.length();
                return value;
            }
            throw parseError("Expected literal: " + text);
        }

        private Number parseNumber() {
            final int start = pos;
            if (pos < src.length() && src.charAt(pos) == '-') pos++;
            while (pos < src.length() && Character.isDigit(src.charAt(pos))) pos++;
            if (pos < src.length() && src.charAt(pos) == '.') {
                pos++;
                while (pos < src.length() && Character.isDigit(src.charAt(pos))) pos++;
            }
            if (pos < src.length() && (src.charAt(pos) == 'e' || src.charAt(pos) == 'E')) {
                pos++;
                if (pos < src.length() && (src.charAt(pos) == '+' || src.charAt(pos) == '-')) pos++;
                while (pos < src.length() && Character.isDigit(src.charAt(pos))) pos++;
            }
            final String num = src.substring(start, pos);
            return Double.parseDouble(num);
        }

        private void skipWhitespace() {
            while (pos < src.length() && Character.isWhitespace(src.charAt(pos))) pos++;
        }

        private char peek() {
            return pos < src.length() ? src.charAt(pos) : 0;
        }

        private void expect(final char c) {
            if (pos >= src.length() || src.charAt(pos) != c) {
                throw parseError("Expected '" + c + "'");
            }
            pos++;
        }

        private IllegalArgumentException parseError(final String msg) {
            return new IllegalArgumentException(msg + " at position " + pos + " in: " + src);
        }
    }
}
