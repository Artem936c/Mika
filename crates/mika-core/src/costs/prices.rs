//! Prices of one model and the money a call costs at them.

use serde::{Deserialize, Serialize};

use super::{CostKind, ManualPrice, PaidCall, Tokens};

/// Money per ONE token, cached token, character, second, picture or request.
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct UnitPrices {
    pub input: Option<f64>,
    pub output: Option<f64>,
    pub cached: Option<f64>,
    pub chars: Option<f64>,
    pub second: Option<f64>,
    pub image: Option<f64>,
    pub request: Option<f64>,
}

impl UnitPrices {
    pub fn is_empty(&self) -> bool {
        *self == UnitPrices::default()
    }

    /// Money of one call, or `None` when these prices do not cover it.
    pub fn cost(&self, call: &PaidCall) -> Option<f64> {
        self.cost_n(call, 1)
    }

    /// Money of `requests` calls whose units are summed up in `call`.
    pub fn cost_n(&self, call: &PaidCall, requests: u64) -> Option<f64> {
        let by_tokens = |t: Tokens| -> Option<f64> {
            let input = self.input?;
            let output = if t.output == 0 {
                0.0
            } else {
                t.output as f64 * self.output?
            };
            Some(t.input as f64 * input + t.cached as f64 * self.cached.unwrap_or(input) + output)
        };
        let tokens = || call.tokens.and_then(by_tokens);
        // A per-unit price needs its units: a call of unknown length is left
        // without a price, never counted as free.
        let base = match call.kind {
            CostKind::Chat | CostKind::Vision | CostKind::Embedding => tokens(),
            CostKind::Speech => match self.chars {
                Some(price) if call.chars > 0 => Some(call.chars as f64 * price),
                _ => tokens(),
            },
            CostKind::Transcription => match self.second {
                Some(price) if call.seconds > 0.0 => Some(call.seconds * price),
                _ => tokens(),
            },
            CostKind::Image => match self.image {
                Some(price) if call.images > 0 => Some(call.images as f64 * price),
                _ => tokens(),
            },
        };
        let per_request = self.request.map(|r| r * requests as f64);
        match (base, per_request) {
            (Some(b), r) => Some(b + r.unwrap_or(0.0)),
            (None, Some(r)) => Some(r),
            (None, None) => None,
        }
    }

    /// Prices the user typed in, per one unit. A field that is 0 is not set.
    pub fn from_manual(m: &ManualPrice) -> UnitPrices {
        let set = |value: f64, per: f64| (value.is_finite() && value > 0.0).then(|| value / per);
        UnitPrices {
            input: set(m.input_mtok, 1e6),
            output: set(m.output_mtok, 1e6),
            cached: None,
            chars: set(m.per_kchar, 1000.0),
            second: set(m.per_minute, 60.0),
            image: set(m.per_image, 1.0),
            request: None,
        }
    }
}

/// Where the price of a model comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PriceSource {
    /// The provider's model list.
    List,
    /// The user typed it in.
    Manual,
    None,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(kind: CostKind) -> PaidCall {
        PaidCall::new(kind, "https://routerai.ru/api/v1", "m")
    }

    fn close(a: Option<f64>, b: f64) -> bool {
        a.is_some_and(|a| (a - b).abs() < 1e-9)
    }

    const TOKENS: Tokens = Tokens {
        input: 1000,
        cached: 200,
        output: 300,
    };

    #[test]
    fn tokens_are_priced_with_and_without_a_cached_price() {
        let mut p = UnitPrices {
            input: Some(0.002),
            output: Some(0.01),
            ..Default::default()
        };
        // Without a cached price the cached tokens cost as input.
        for kind in [CostKind::Chat, CostKind::Vision] {
            assert!(close(
                p.cost(&call(kind).tokens(TOKENS)),
                1000.0 * 0.002 + 200.0 * 0.002 + 300.0 * 0.01
            ));
        }
        p.cached = Some(0.0005);
        assert!(close(
            p.cost(&call(CostKind::Chat).tokens(TOKENS)),
            1000.0 * 0.002 + 200.0 * 0.0005 + 300.0 * 0.01
        ));
        // No usage, no price.
        assert_eq!(p.cost(&call(CostKind::Chat)), None);
        assert!(!p.is_empty() && UnitPrices::default().is_empty());
    }

    #[test]
    fn output_tokens_need_an_output_price() {
        let p = UnitPrices {
            input: Some(0.002),
            ..Default::default()
        };
        assert_eq!(p.cost(&call(CostKind::Chat).tokens(TOKENS)), None);
        // An embedding has no output.
        let embedding = call(CostKind::Embedding).tokens(Tokens {
            input: 500,
            ..Default::default()
        });
        assert!(close(p.cost(&embedding), 1.0));
        // And without an input price there is no token price at all.
        let p = UnitPrices {
            output: Some(0.01),
            ..Default::default()
        };
        assert_eq!(p.cost(&embedding), None);
    }

    #[test]
    fn speech_is_priced_by_characters_or_by_tokens() {
        let by_chars = UnitPrices {
            chars: Some(0.001),
            ..Default::default()
        };
        assert!(close(
            by_chars.cost(&call(CostKind::Speech).chars(840)),
            0.84
        ));
        // Token prices alone say nothing about a phrase whose tokens are unknown.
        let by_tokens = UnitPrices {
            input: Some(0.00005),
            output: Some(0.0006),
            ..Default::default()
        };
        assert_eq!(by_tokens.cost(&call(CostKind::Speech).chars(840)), None);
        assert!(close(
            by_tokens.cost(&call(CostKind::Speech).chars(840).tokens(TOKENS)),
            1200.0 * 0.00005 + 300.0 * 0.0006
        ));
        // No characters: the per-character price is not used.
        assert_eq!(by_chars.cost(&call(CostKind::Speech)), None);
    }

    #[test]
    fn recognition_is_priced_by_seconds_and_unknown_length_has_no_price() {
        let p = UnitPrices {
            second: Some(0.00036),
            ..Default::default()
        };
        assert!(close(
            p.cost(&call(CostKind::Transcription).seconds(250.0)),
            0.09
        ));
        // A sound that is not WAV and no `usage.seconds`: unpriced, not free.
        assert_eq!(p.cost(&call(CostKind::Transcription).seconds(0.0)), None);
        let with_tokens = UnitPrices {
            input: Some(0.001),
            ..p
        };
        assert!(close(
            with_tokens.cost(&call(CostKind::Transcription).seconds(0.0).tokens(Tokens {
                input: 100,
                ..Default::default()
            })),
            0.1
        ));
    }

    #[test]
    fn a_picture_is_priced_by_the_piece_and_a_request_price_is_added() {
        let p = UnitPrices {
            image: Some(0.37),
            ..Default::default()
        };
        assert!(close(p.cost(&call(CostKind::Image).images(2)), 0.74));
        assert_eq!(p.cost(&call(CostKind::Image)), None);
        let p = UnitPrices {
            image: Some(0.37),
            request: Some(0.01),
            ..Default::default()
        };
        assert!(close(p.cost(&call(CostKind::Image).images(1)), 0.38));
        assert!(close(p.cost_n(&call(CostKind::Image).images(3), 3), 1.14));
        // Only a request price: it is the whole price.
        let p = UnitPrices {
            request: Some(0.02),
            ..Default::default()
        };
        assert!(close(p.cost(&call(CostKind::Chat)), 0.02));
        assert!(close(p.cost_n(&call(CostKind::Chat), 5), 0.1));
        assert_eq!(UnitPrices::default().cost(&call(CostKind::Chat)), None);
    }

    #[test]
    fn a_typed_price_becomes_a_price_per_unit() {
        let p = UnitPrices::from_manual(&ManualPrice {
            provider: "api.openai.com".into(),
            model: "gpt-4o-mini".into(),
            currency: "USD".into(),
            input_mtok: 0.15,
            output_mtok: 0.6,
            per_kchar: 15.0,
            per_minute: 0.36,
            per_image: 4.0,
        });
        assert!(close(p.input, 0.15e-6) && close(p.output, 0.6e-6));
        assert!(close(p.chars, 0.015) && close(p.second, 0.006) && close(p.image, 4.0));
        assert_eq!((p.cached, p.request), (None, None));
        assert!(UnitPrices::from_manual(&ManualPrice::default()).is_empty());
    }

    #[test]
    fn prices_are_stored_as_json_and_read_back() {
        let p = UnitPrices {
            input: Some(2.68e-6),
            output: Some(0.0),
            ..Default::default()
        };
        let json = serde_json::to_string(&p).unwrap();
        assert_eq!(serde_json::from_str::<UnitPrices>(&json).unwrap(), p);
        assert_eq!(
            serde_json::from_str::<UnitPrices>(r#"{"second":0.00036}"#).unwrap(),
            UnitPrices {
                second: Some(0.00036),
                ..Default::default()
            }
        );
        assert_eq!(serde_json::to_value(PriceSource::Manual).unwrap(), "manual");
    }
}
