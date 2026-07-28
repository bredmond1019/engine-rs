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

use crate::node::NodeError;
use crate::policy::PolicyConfigSource;

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

/// A first-engagement shape a client can be quoted. Purely descriptive —
/// this crate never converts between engagements, only looks one up on a
/// [`RateSheet`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum EngagementKind {
    Diagnostic,
    Project,
    Retainer,
}

/// How a [`MoneyRange`] is billed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EngagementBasis {
    Fixed,
    PerMonth,
    PerHour,
}

/// A priced range in one currency. `currency` is carried explicitly (rather
/// than inferred from context) so a `MoneyRange` is self-describing wherever
/// it travels — e.g. once embedded in `FirstEngagement.investment`.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct MoneyRange {
    pub currency: Currency,
    pub min: f64,
    pub max: f64,
    pub basis: EngagementBasis,
}

/// One locale's complete price list — everything `business/docs/rates.md`
/// defines for a single sheet. `hourly_floor` is internal scoping-only
/// guidance, not a client-facing engagement, so it is a plain figure rather
/// than a [`MoneyRange`].
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct RateSheet {
    pub currency: Currency,
    pub diagnostic: MoneyRange,
    pub project: MoneyRange,
    pub retainer: MoneyRange,
    pub hourly_floor: f64,
}

impl RateSheet {
    /// FIREWALL INVARIANT enforcement: every [`MoneyRange`] on this sheet
    /// must be denominated in the sheet's own `currency`. A sheet built from
    /// a malformed `harness.json` section that mixes currencies is refused
    /// rather than silently accepted.
    fn validate(&self) -> Result<(), NodeError> {
        for (label, range) in [
            ("diagnostic", &self.diagnostic),
            ("project", &self.project),
            ("retainer", &self.retainer),
        ] {
            if range.currency != self.currency {
                return Err(NodeError::new(format!(
                    "rate sheet currency mismatch: sheet is {:?} but {label} range is {:?}",
                    self.currency, range.currency
                )));
            }
        }
        Ok(())
    }
}

/// The two-sheet, firewalled rate card (EN.4.F). [`RateCard::sheet`] is the
/// ONLY accessor — there is deliberately no method returning both sheets or
/// converting between them. See the firewall invariant at the top of this
/// module.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct RateCard {
    pt_br: RateSheet,
    en_us: RateSheet,
}

impl RateCard {
    /// The rate sheet for `locale`. The only way to read a figure out of a
    /// `RateCard` — by design, there is no sibling method that hands back
    /// the other sheet in the same call.
    #[must_use]
    pub fn sheet(&self, locale: Locale) -> &RateSheet {
        match locale {
            Locale::PtBr => &self.pt_br,
            Locale::EnUs => &self.en_us,
        }
    }

    fn validate(&self) -> Result<(), NodeError> {
        self.pt_br.validate()?;
        self.en_us.validate()?;
        Ok(())
    }

    /// Read the `rate_card` section of `harness.json`, resolved through
    /// `source` (mirroring `policy::read_harness_policy_defaults_from`'s
    /// mechanism). An absent section — no file, no path
    /// ([`PolicyConfigSource::Builtin`]), or no `rate_card` key — resolves
    /// to [`RateCard::default`] so an unconfigured repo is behavior-stable.
    /// A *present but malformed* section is a hard error (strict-read
    /// posture, mirroring `resolved_policy_strict`): this crate never falls
    /// back to `Default` silently once a human has started editing the
    /// section.
    pub fn load_from(source: &PolicyConfigSource) -> Result<RateCard, NodeError> {
        let default = RateCard::default();

        let Some(harness_path) = source.harness_path() else {
            return Ok(default);
        };
        if !harness_path.exists() {
            return Ok(default);
        }

        let raw = std::fs::read_to_string(&harness_path).map_err(|err| {
            NodeError::new(format!("failed to read {}: {err}", harness_path.display()))
        })?;
        let harness: serde_json::Value = serde_json::from_str(&raw).map_err(|err| {
            NodeError::new(format!("failed to parse {}: {err}", harness_path.display()))
        })?;

        let Some(rate_card_value) = harness.get("rate_card") else {
            return Ok(default);
        };

        let card: RateCard = serde_json::from_value(rate_card_value.clone()).map_err(|err| {
            NodeError::new(format!(
                "failed to parse rate_card section of {}: {err}",
                harness_path.display()
            ))
        })?;
        card.validate()?;
        Ok(card)
    }
}

impl Default for RateCard {
    /// The ported `business/docs/rates.md` numbers, verbatim (see the spec's
    /// Notes table). Both sheets carry rate-card provenance caveats — see
    /// the `_comment` beside `rate_card` in `planning/harness.json`.
    fn default() -> Self {
        RateCard {
            pt_br: RateSheet {
                currency: Currency::Brl,
                diagnostic: MoneyRange {
                    currency: Currency::Brl,
                    min: 3_000.0,
                    max: 6_000.0,
                    basis: EngagementBasis::Fixed,
                },
                project: MoneyRange {
                    currency: Currency::Brl,
                    min: 10_000.0,
                    max: 30_000.0,
                    basis: EngagementBasis::Fixed,
                },
                retainer: MoneyRange {
                    currency: Currency::Brl,
                    min: 3_500.0,
                    max: 7_000.0,
                    basis: EngagementBasis::PerMonth,
                },
                hourly_floor: 200.0,
            },
            en_us: RateSheet {
                currency: Currency::Usd,
                diagnostic: MoneyRange {
                    currency: Currency::Usd,
                    min: 1_000.0,
                    max: 2_000.0,
                    basis: EngagementBasis::Fixed,
                },
                project: MoneyRange {
                    currency: Currency::Usd,
                    min: 5_000.0,
                    max: 15_000.0,
                    basis: EngagementBasis::Fixed,
                },
                retainer: MoneyRange {
                    currency: Currency::Usd,
                    min: 2_000.0,
                    max: 4_000.0,
                    basis: EngagementBasis::PerMonth,
                },
                hourly_floor: 85.0,
            },
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

    // --- RateCard --------------------------------------------------------

    use std::sync::atomic::{AtomicU64, Ordering};
    static TMP_COUNTER: AtomicU64 = AtomicU64::new(0);

    fn temp_harness_file(contents: &serde_json::Value) -> std::path::PathBuf {
        let n = TMP_COUNTER.fetch_add(1, Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!(
            "engine-core-locale-rate-card-test-{}-{n}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        let path = dir.join("harness.json");
        std::fs::write(&path, serde_json::to_string_pretty(contents).unwrap())
            .expect("write fixture harness.json");
        path
    }

    fn default_rate_card_json() -> serde_json::Value {
        serde_json::to_value(RateCard::default()).unwrap()
    }

    #[test]
    fn rate_card_builtin_default_matches_ported_table() {
        let card = RateCard::default();
        let pt = card.sheet(Locale::PtBr);
        assert_eq!(pt.currency, Currency::Brl);
        assert_eq!(pt.diagnostic.min, 3_000.0);
        assert_eq!(pt.diagnostic.max, 6_000.0);
        assert_eq!(pt.project.min, 10_000.0);
        assert_eq!(pt.project.max, 30_000.0);
        assert_eq!(pt.retainer.min, 3_500.0);
        assert_eq!(pt.retainer.max, 7_000.0);
        assert_eq!(pt.hourly_floor, 200.0);

        let en = card.sheet(Locale::EnUs);
        assert_eq!(en.currency, Currency::Usd);
        assert_eq!(en.diagnostic.min, 1_000.0);
        assert_eq!(en.diagnostic.max, 2_000.0);
        assert_eq!(en.project.min, 5_000.0);
        assert_eq!(en.project.max, 15_000.0);
        assert_eq!(en.retainer.min, 2_000.0);
        assert_eq!(en.retainer.max, 4_000.0);
        assert_eq!(en.hourly_floor, 85.0);
    }

    #[test]
    fn rate_card_builtin_source_returns_default() {
        let card = RateCard::load_from(&PolicyConfigSource::Builtin).expect("load should succeed");
        assert_eq!(card, RateCard::default());
    }

    #[test]
    fn rate_card_absent_file_returns_default() {
        let dir = std::env::temp_dir().join(format!(
            "engine-core-locale-rate-card-absent-{}",
            std::process::id()
        ));
        let missing = dir.join("does-not-exist.json");
        let card = RateCard::load_from(&PolicyConfigSource::HarnessFile(missing))
            .expect("load should succeed");
        assert_eq!(card, RateCard::default());
    }

    #[test]
    fn rate_card_absent_section_returns_default() {
        let path = temp_harness_file(&serde_json::json!({ "other_section": {} }));
        let card = RateCard::load_from(&PolicyConfigSource::HarnessFile(path))
            .expect("load should succeed");
        assert_eq!(card, RateCard::default());
    }

    #[test]
    fn rate_card_loads_expected_numbers_from_fixture() {
        let path = temp_harness_file(&serde_json::json!({
            "rate_card": default_rate_card_json(),
        }));
        let card =
            RateCard::load_from(&PolicyConfigSource::HarnessFile(path)).expect("should load");
        assert_eq!(card, RateCard::default());
    }

    #[test]
    fn rate_card_malformed_section_errors() {
        let mut malformed = default_rate_card_json();
        // Corrupt a required field's type so deserialization fails.
        malformed["pt_br"]["diagnostic"]["min"] = serde_json::json!("not-a-number");
        let path = temp_harness_file(&serde_json::json!({ "rate_card": malformed }));
        let err = RateCard::load_from(&PolicyConfigSource::HarnessFile(path))
            .expect_err("malformed section must error, not fall back to default");
        assert!(err.message.contains("rate_card"));
    }

    #[test]
    fn rate_card_currency_mismatch_errors() {
        let mut mismatched = default_rate_card_json();
        // Give the pt_br sheet's diagnostic range the wrong currency.
        mismatched["pt_br"]["diagnostic"]["currency"] = serde_json::json!("USD");
        let path = temp_harness_file(&serde_json::json!({ "rate_card": mismatched }));
        let err = RateCard::load_from(&PolicyConfigSource::HarnessFile(path))
            .expect_err("currency mismatch must error");
        assert!(err.message.contains("currency mismatch"));
    }

    #[test]
    fn rate_card_sheet_currency_is_locale_pure() {
        let card = RateCard::default();
        assert_eq!(card.sheet(Locale::PtBr).currency, Currency::Brl);
        assert_eq!(card.sheet(Locale::EnUs).currency, Currency::Usd);
    }

    #[test]
    fn rate_card_every_range_in_a_sheet_carries_that_sheets_currency() {
        let card = RateCard::default();
        for locale in [Locale::PtBr, Locale::EnUs] {
            let sheet = card.sheet(locale);
            for range in [&sheet.diagnostic, &sheet.project, &sheet.retainer] {
                assert_eq!(range.currency, sheet.currency);
            }
        }
    }

    #[test]
    fn pt_br_lookup_never_returns_a_usd_figure() {
        let card = RateCard::default();
        let sheet = card.sheet(Locale::PtBr);
        assert_ne!(sheet.currency, Currency::Usd);
        for range in [&sheet.diagnostic, &sheet.project, &sheet.retainer] {
            assert_ne!(range.currency, Currency::Usd);
        }
    }
}
