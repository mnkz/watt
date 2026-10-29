use crate::model::Parts;

/// Public API rates as of October 29, 2026, in dollars per million tokens.
/// Claude cache writes follow the Anthropic card (5 min / 1 h).
/// Past the long-context tier, input and output use the long rate.
#[derive(Clone, Copy)]
struct Card {
    input: f64,
    cached: f64,
    write_5m: f64,
    write_1h: f64,
    output: f64,
    long_at: Option<u64>,
    long_input: f64,
    long_cached: f64,
    long_output: f64,
    /// OpenAI does not bill cache writes separately: they are priced as input.
    write_like_input: bool,
}

impl Card {
    const fn flat(input: f64, cached: f64, output: f64) -> Self {
        Self {
            input,
            cached,
            write_5m: input,
            write_1h: input,
            output,
            long_at: None,
            long_input: input,
            long_cached: cached,
            long_output: output,
            write_like_input: true,
        }
    }

    const fn claude(input: f64, cached: f64, write_5m: f64, write_1h: f64, output: f64) -> Self {
        Self {
            input,
            cached,
            write_5m,
            write_1h,
            output,
            long_at: None,
            long_input: input,
            long_cached: cached,
            long_output: output,
            write_like_input: false,
        }
    }

    const fn long(
        input: f64,
        cached: f64,
        output: f64,
        at: u64,
        long_input: f64,
        long_cached: f64,
        long_output: f64,
    ) -> Self {
        Self {
            input,
            cached,
            write_5m: input,
            write_1h: input,
            output,
            long_at: Some(at),
            long_input,
            long_cached,
            long_output,
            write_like_input: true,
        }
    }
}

pub fn price_usd(model: &str, parts: Parts) -> f64 {
    let card = card_for(model);
    let over = card.long_at.is_some_and(|n| parts.prompt > n);
    let (input, cached, output) = if over {
        (card.long_input, card.long_cached, card.long_output)
    } else {
        (card.input, card.cached, card.output)
    };
    let (write_5m, write_1h) = if card.write_like_input {
        (input, input)
    } else {
        (card.write_5m, card.write_1h)
    };
    (parts.fresh as f64 * input
        + parts.cache_read as f64 * cached
        + parts.write_5m as f64 * write_5m
        + parts.write_1h as f64 * write_1h
        + parts.output as f64 * output)
        / 1_000_000.0
}

pub fn usd_to_micros(usd: f64) -> u64 {
    if usd <= 0.0 || !usd.is_finite() {
        0
    } else {
        (usd * 1_000_000.0).round() as u64
    }
}

fn card_for(model: &str) -> Card {
    let m = model.to_ascii_lowercase();
    let has = |pat: &str| m.contains(pat);

    if has("claude") {
        return claude_card(&m);
    }
    if has("gpt") || has("codex") || has("o3") || has("o4") {
        return openai_card(&m);
    }
    if has("grok") {
        return grok_card(&m);
    }
    if has("gemini") || has("argon") {
        return gemini_card(&m);
    }
    if has("cursor") {
        return Card::flat(2.0, 0.20, 10.0);
    }
    Card::flat(2.0, 0.20, 10.0)
}

fn claude_card(m: &str) -> Card {
    let has = |pat: &str| m.contains(pat);
    if has("fable-5-1") || has("fable-5.1") {
        return Card::claude(10.0, 0.25, 12.50, 20.0, 50.0);
    }
    if has("fable") || has("mythos") {
        let cached = if has("5-1") || has("5.1") { 0.25 } else { 1.0 };
        return Card::claude(10.0, cached, 12.50, 20.0, 50.0);
    }
    if has("opus-5-5") || has("opus-5.5") {
        return Card::claude(4.0, 0.20, 5.0, 8.0, 20.0);
    }
    if has("opus-4-1") || has("opus-4.1") || m.ends_with("opus-4") {
        return Card::claude(15.0, 1.50, 18.75, 30.0, 75.0);
    }
    if has("opus") {
        return Card::claude(5.0, 0.50, 6.25, 10.0, 25.0);
    }
    if has("sonnet-5-5") || has("sonnet-5.5") {
        return Card::claude(2.0, 0.10, 2.50, 4.0, 10.0);
    }
    if has("sonnet-5") || has("sonnet-5.") {
        return Card::claude(2.0, 0.20, 2.50, 4.0, 10.0);
    }
    if has("sonnet") {
        return Card::claude(3.0, 0.30, 3.75, 6.0, 15.0);
    }
    if has("haiku-5") {
        return Card::claude(0.10, 0.01, 0.125, 0.20, 0.50);
    }
    if has("haiku") {
        return Card::claude(1.0, 0.10, 1.25, 2.0, 5.0);
    }
    Card::claude(5.0, 0.50, 6.25, 10.0, 25.0)
}

fn openai_card(m: &str) -> Card {
    let has = |pat: &str| m.contains(pat);
    // Above 272,000 prompt tokens: input ×2, output ×1.5.
    let long = |input: f64, cached: f64, output: f64| {
        Card::long(
            input,
            cached,
            output,
            272_000,
            input * 2.0,
            cached * 2.0,
            output * 1.5,
        )
    };
    if has("astra") {
        return long(10.0, 1.0, 50.0);
    }
    if has("5.6") && has("sol") {
        return long(4.0, 0.40, 20.0);
    }
    if has("sol") {
        return long(2.0, 0.10, 10.0);
    }
    if has("terra") {
        return long(1.0, 0.05, 5.0);
    }
    if has("5.6") && has("luna") {
        return long(0.20, 0.02, 1.20);
    }
    if has("luna") || has("nano") {
        return long(0.10, 0.01, 0.50);
    }
    if has("5.3") || has("5.2") {
        return long(1.75, 0.175, 14.0);
    }
    if has("5.4") && has("mini") {
        return long(0.75, 0.075, 4.50);
    }
    if has("mini") {
        return long(0.25, 0.025, 2.0);
    }
    if has("gpt-5") || has("gpt-4") || has("codex") {
        return long(1.25, 0.125, 10.0);
    }
    long(2.0, 0.20, 10.0)
}

fn grok_card(m: &str) -> Card {
    let has = |pat: &str| m.contains(pat);
    if has("build") {
        return Card::flat(1.0, 0.20, 2.0);
    }
    if has("4.1") || has("4-1") || has("fast") {
        return Card::flat(0.20, 0.05, 0.50);
    }
    if has("4.3") || has("4-3") {
        return Card::long(1.25, 0.20, 2.50, 200_000, 2.50, 0.40, 5.0);
    }
    // Grok 4.5 / 4.6 / 4.7: 2 / 0.50 / 6, doubled past 200,000.
    Card::long(2.0, 0.50, 6.0, 200_000, 4.0, 1.0, 12.0)
}

fn gemini_card(m: &str) -> Card {
    let has = |pat: &str| m.contains(pat);
    if has("argon") || has("gemini-4") {
        // Introductory rate through November 30, 2026.
        return Card::flat(2.0, 0.10, 10.0);
    }
    if has("3.1") && has("pro") || has("3-pro") {
        return Card::long(2.0, 0.20, 12.0, 200_000, 4.0, 0.40, 18.0);
    }
    if has("2.5") && has("pro") {
        return Card::long(1.25, 0.125, 10.0, 200_000, 2.50, 0.25, 15.0);
    }
    if has("flash-lite") || has("flash_lite") {
        return Card::flat(0.30, 0.03, 2.50);
    }
    if has("3.8") || has("3.6") || has("3.7") {
        return Card::flat(0.75, 0.075, 3.75);
    }
    if has("flash") {
        return Card::flat(0.50, 0.05, 3.0);
    }
    Card::flat(2.0, 0.20, 12.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opus_55_splits_cache_write() {
        let parts = Parts {
            fresh: 2,
            cache_read: 31_372,
            write_5m: 0,
            write_1h: 19_286,
            output: 677,
            prompt: 50_660,
        };
        let usd = price_usd("claude-opus-5-5", parts);
        let expect = (2.0 * 4.0 + 31_372.0 * 0.20 + 19_286.0 * 8.0 + 677.0 * 20.0) / 1_000_000.0;
        assert!((usd - expect).abs() < 1e-9, "{usd} != {expect}");
    }

    #[test]
    fn sol_uses_cached_rate_and_long_context() {
        let small = Parts {
            fresh: 1_000,
            cache_read: 9_000,
            write_5m: 0,
            write_1h: 0,
            output: 100,
            prompt: 10_000,
        };
        let usd = price_usd("gpt-6.1-sol", small);
        let expect = (1_000.0 * 2.0 + 9_000.0 * 0.10 + 100.0 * 10.0) / 1_000_000.0;
        assert!((usd - expect).abs() < 1e-9);

        let long = Parts {
            fresh: 100_000,
            cache_read: 200_000,
            write_5m: 0,
            write_1h: 0,
            output: 1_000,
            prompt: 300_000,
        };
        let usd = price_usd("gpt-6.1-sol", long);
        let expect = (100_000.0 * 4.0 + 200_000.0 * 0.20 + 1_000.0 * 15.0) / 1_000_000.0;
        assert!((usd - expect).abs() < 1e-9, "{usd} != {expect}");
    }

    #[test]
    fn grok_build_is_cheaper_than_flagship() {
        let parts = Parts {
            fresh: 1_000_000,
            cache_read: 0,
            write_5m: 0,
            write_1h: 0,
            output: 0,
            prompt: 1_000_000,
        };
        assert!((price_usd("grok-4.7-build", parts) - 1.0).abs() < 1e-9);
        assert!((price_usd("grok-4.7", parts) - 4.0).abs() < 1e-9);
    }
}
