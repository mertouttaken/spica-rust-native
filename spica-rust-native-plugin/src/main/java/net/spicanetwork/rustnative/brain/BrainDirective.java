package net.spicanetwork.rustnative.brain;

import java.util.Locale;

/**
 * Immutable control directive produced either by the external {@code spica-launcher} Rust Brain
 * daemon over NDJSON TCP IPC or by the in-process {@code spica-native} Rust Governor fallback.
 */
public record BrainDirective(
    String state,
    int stateCode,
    float ewmaMspt,
    int viewDistance,
    int simDistance,
    float fullAiRange,
    float hibernateRange,
    boolean itemMergeEnabled,
    float itemMergeRadius,
    String reason,
    boolean fromLauncher
) {

    public static final int STATE_GREEN = 0;
    public static final int STATE_YELLOW = 1;
    public static final int STATE_ORANGE = 2;
    public static final int STATE_RED = 3;
    public static final int STATE_EMERGENCY = 4;

    public static String stateNameForCode(int code) {
        return switch (code) {
            case STATE_GREEN -> "GREEN";
            case STATE_YELLOW -> "YELLOW";
            case STATE_ORANGE -> "ORANGE";
            case STATE_RED -> "RED";
            case STATE_EMERGENCY -> "EMERGENCY";
            default -> "UNKNOWN";
        };
    }

    public static int codeForStateName(String name) {
        if (name == null || name.isBlank()) {
            return STATE_GREEN;
        }
        return switch (name.trim().toUpperCase(Locale.ROOT)) {
            case "GREEN" -> STATE_GREEN;
            case "YELLOW" -> STATE_YELLOW;
            case "ORANGE" -> STATE_ORANGE;
            case "RED" -> STATE_RED;
            case "EMERGENCY" -> STATE_EMERGENCY;
            default -> STATE_GREEN;
        };
    }

    public static BrainDirective safeDefault(
        float ewmaMspt,
        int viewDistance,
        int simDistance,
        float fullAiRange,
        float hibernateRange,
        String reason
    ) {
        return new BrainDirective(
            "GREEN",
            STATE_GREEN,
            Float.isFinite(ewmaMspt) && ewmaMspt >= 0f ? ewmaMspt : 0f,
            viewDistance,
            simDistance,
            fullAiRange,
            hibernateRange,
            false,
            3.5f,
            reason != null ? reason : "default",
            false
        );
    }
}
