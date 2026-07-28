//! Client locale + currency (EN.4.F).
//!
//! One `Locale` carries two things the diagnostic funnel needs and did not
//! have: the language a run's prose is written in, and — via `Currency` —
//! which of `business/docs/rates.md`'s two rate sheets applies.
//!
//! Exactly two locales exist because `rates.md` defines exactly two sheets:
//! BRL for Brazil, USD for "US / EU / funded startups". A third locale would
//! need a third sheet, not just a third language tag.
//!
//! FIREWALL INVARIANT (load-bearing, see `rates.md`): the two sheets are
//! "never quoted in the same conversation, never cross-converted". This
//! module therefore defines NO conversion between `Brl` and `Usd` — no rate
//! constant, no helper, no test. That absence is the feature.
//!
//! `Locale` is a per-client attribute, not a cost/latency/quality knob — per
//! CLAUDE.md rule 6 it lives on event schemas, never on `ProposalGeneratorPolicy`
//! or a named profile bundle.

use serde::{Deserialize, Serialize};

/// A client's market segmentation: which language a run's prose is written
/// in, and (via [`Locale::currency`]) which rate sheet applies.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum Locale {
    #[serde(rename = "pt-BR")]
    #[default]
    PtBr,
    #[serde(rename = "en-US")]
    EnUs,
}

/// The currency a rate sheet is denominated in. See the firewall invariant
/// at the top of this module: no conversion between these variants exists
/// anywhere in this crate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Currency {
    #[serde(rename = "BRL")]
    Brl,
    #[serde(rename = "USD")]
    Usd,
}

impl Locale {
    /// Total, infallible mapping to the rate sheet's currency.
    #[must_use]
    pub fn currency(self) -> Currency {
        match self {
            Locale::PtBr => Currency::Brl,
            Locale::EnUs => Currency::Usd,
        }
    }

    /// The language a model should write this run's prose in. Spliced into
    /// per-run prompt bodies by the writer/research/intake nodes — never
    /// into a `STABLE_SYSTEM_PROMPT` (CLAUDE.md rule 6, cache breakpoints).
    #[must_use]
    pub fn language_name(self) -> &'static str {
        match self {
            Locale::PtBr => "Brazilian Portuguese (pt-BR)",
            Locale::EnUs => "English (en-US)",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn locale_serializes_to_bcp47_tags() {
        assert_eq!(serde_json::to_string(&Locale::PtBr).unwrap(), "\"pt-BR\"");
        assert_eq!(serde_json::to_string(&Locale::EnUs).unwrap(), "\"en-US\"");
    }

    #[test]
    fn locale_round_trips_through_serde() {
        for locale in [Locale::PtBr, Locale::EnUs] {
            let json = serde_json::to_string(&locale).unwrap();
            let back: Locale = serde_json::from_str(&json).unwrap();
            assert_eq!(back, locale);
        }
    }

    #[test]
    fn locale_default_is_pt_br() {
        assert_eq!(Locale::default(), Locale::PtBr);
    }

    #[test]
    fn unknown_locale_tag_fails_to_deserialize() {
        assert!(serde_json::from_str::<Locale>("\"en-GB\"").is_err());
    }

    #[test]
    fn currency_maps_from_locale() {
        assert_eq!(Locale::PtBr.currency(), Currency::Brl);
        assert_eq!(Locale::EnUs.currency(), Currency::Usd);
    }

    #[test]
    fn currency_serializes_to_iso_codes() {
        assert_eq!(serde_json::to_string(&Currency::Brl).unwrap(), "\"BRL\"");
        assert_eq!(serde_json::to_string(&Currency::Usd).unwrap(), "\"USD\"");
    }

    #[test]
    fn language_name_is_non_empty_and_distinct() {
        let pt = Locale::PtBr.language_name();
        let en = Locale::EnUs.language_name();
        assert!(!pt.is_empty());
        assert!(!en.is_empty());
        assert_ne!(pt, en);
    }
}
