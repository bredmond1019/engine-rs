//! Event schema (`DeliverableRenderEventSchema`) for the `DELIVERABLE_RENDER`
//! workflow, plus the `<company-slug>` basename derivation both
//! `RenderDeliverableNode` and `RenderPdfNode` share.
//!
//! [`DeliverableRenderEventSchema`] carries the `AutomationRoadmap` INLINE —
//! this workflow renders a roadmap that already exists (produced upstream
//! by `PROPOSAL_GENERATOR`, `EN.4.C`), it does not build one. Importing
//! `AutomationRoadmap` from `proposal_generator::schema` is deliberate: that
//! struct's own doc comment says the name is load-bearing *because*
//! `EN.4.D` imports it.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::locale::Locale;
use crate::workflows::proposal_generator::schema::AutomationRoadmap;

use super::policy::PartialDeliverableRenderPolicy;

/// Fallback basename component used when the roadmap carries no
/// `situation` (and therefore no `company_name`) to derive a slug from, or
/// when the derived slug would otherwise be empty (e.g. a `company_name`
/// made up entirely of punctuation). Documented and stable rather than a
/// panic — `RenderDeliverableNode` / `RenderPdfNode` must still be able to
/// name their output files.
pub const FALLBACK_COMPANY_SLUG: &str = "deliverable";

/// Inbound event schema for the `DELIVERABLE_RENDER` workflow.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DeliverableRenderEventSchema {
    /// The roadmap to render, produced upstream by `PROPOSAL_GENERATOR`
    /// (`EN.4.C`) and passed inline rather than re-fetched — this workflow
    /// never looks the roadmap up itself.
    pub roadmap: AutomationRoadmap,
    /// The locale this run's rendered chrome and currency must use.
    /// `RenderDeliverableNode` refuses when this disagrees with
    /// `roadmap.authored_locale` rather than emitting a mixed-language
    /// document (`EN.4.D`, amended 2026-07-28).
    #[serde(default)]
    pub locale: Locale,
    /// Directory both output artifacts (`<company-slug>-roadmap.md` and
    /// `.pdf`) are written under.
    pub output_dir: PathBuf,
    /// Per-run policy override — the highest-precedence layer in
    /// `policy::resolve`'s four-layer merge (`EN.4.D` task 2).
    #[serde(default)]
    pub policy: Option<PartialDeliverableRenderPolicy>,
    /// A named profile bundle (`baseline` / `cheap-fast` / `thorough`) to
    /// resolve against, second-highest precedence after `policy` above.
    #[serde(default)]
    pub profile: Option<String>,
}

/// Derive the `<company-slug>` basename component
/// (`<company-slug>-roadmap.{md,pdf}`) from `roadmap.situation.company_name`
/// — kebab-case, ASCII-folded, lowercase.
///
/// Falls back to [`FALLBACK_COMPANY_SLUG`] when `situation` is absent, or
/// when the derived slug would otherwise be empty (an all-punctuation or
/// empty `company_name`) — never panics.
#[must_use]
pub fn deliverable_slug(roadmap: &AutomationRoadmap) -> String {
    let name = match &roadmap.situation {
        Some(situation) => situation.company_name.as_str(),
        None => return FALLBACK_COMPANY_SLUG.to_string(),
    };
    let slug = kebab_ascii_fold(name);
    if slug.is_empty() {
        FALLBACK_COMPANY_SLUG.to_string()
    } else {
        slug
    }
}

/// ASCII-fold a string (stripping accents from the Latin-1 Supplement +
/// Latin Extended-A ranges relevant to `pt-BR` company names), lowercase
/// it, and kebab-case it: runs of non-alphanumeric characters collapse to a
/// single `-`, and leading/trailing `-` are trimmed.
fn kebab_ascii_fold(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut last_was_dash = true; // suppresses a leading dash
    for ch in input.chars() {
        let folded = fold_ascii(ch);
        match folded {
            Some(c) if c.is_ascii_alphanumeric() => {
                out.push(c.to_ascii_lowercase());
                last_was_dash = false;
            }
            _ => {
                if !last_was_dash {
                    out.push('-');
                    last_was_dash = true;
                }
            }
        }
    }
    if out.ends_with('-') {
        out.pop();
    }
    out
}

/// Fold one character to its closest ASCII equivalent, covering the
/// accented letters that occur in `pt-BR` company names (á/à/â/ã/ä, é/è/ê/ë,
/// í/ì/î/ï, ó/ò/ô/õ/ö, ú/ù/û/ü, ç, ñ, and their uppercase forms). Any
/// character already ASCII, or without a known fold, passes through
/// unchanged — the caller filters non-alphanumerics regardless.
fn fold_ascii(ch: char) -> Option<char> {
    if ch.is_ascii() {
        return Some(ch);
    }
    let folded = match ch {
        'á' | 'à' | 'â' | 'ã' | 'ä' | 'å' => 'a',
        'Á' | 'À' | 'Â' | 'Ã' | 'Ä' | 'Å' => 'A',
        'é' | 'è' | 'ê' | 'ë' => 'e',
        'É' | 'È' | 'Ê' | 'Ë' => 'E',
        'í' | 'ì' | 'î' | 'ï' => 'i',
        'Í' | 'Ì' | 'Î' | 'Ï' => 'I',
        'ó' | 'ò' | 'ô' | 'õ' | 'ö' => 'o',
        'Ó' | 'Ò' | 'Ô' | 'Õ' | 'Ö' => 'O',
        'ú' | 'ù' | 'û' | 'ü' => 'u',
        'Ú' | 'Ù' | 'Û' | 'Ü' => 'U',
        'ç' => 'c',
        'Ç' => 'C',
        'ñ' => 'n',
        'Ñ' => 'N',
        _ => return None,
    };
    Some(folded)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workflows::proposal_generator::schema::SituationAndOpportunity;

    fn roadmap_with_company(company_name: &str) -> AutomationRoadmap {
        AutomationRoadmap {
            situation: Some(SituationAndOpportunity {
                company_name: company_name.to_string(),
                business_type: "retail SMB".to_string(),
                team_size: 5,
                painful_workflow_summary: "manual invoicing".to_string(),
                candidate_count: 3,
            }),
            ..Default::default()
        }
    }

    #[test]
    fn slug_folds_accents_spaces_and_punctuation() {
        let roadmap = roadmap_with_company("Padaria São João");
        assert_eq!(deliverable_slug(&roadmap), "padaria-sao-joao");
    }

    #[test]
    fn slug_collapses_punctuation_runs() {
        let roadmap = roadmap_with_company("Acme, Inc. -- Co.");
        assert_eq!(deliverable_slug(&roadmap), "acme-inc-co");
    }

    #[test]
    fn absent_situation_yields_the_documented_fallback() {
        let roadmap = AutomationRoadmap {
            situation: None,
            ..Default::default()
        };
        assert_eq!(deliverable_slug(&roadmap), FALLBACK_COMPANY_SLUG);
    }

    #[test]
    fn empty_company_name_yields_the_documented_fallback_not_a_panic() {
        let roadmap = roadmap_with_company("");
        assert_eq!(deliverable_slug(&roadmap), FALLBACK_COMPANY_SLUG);
    }

    #[test]
    fn all_punctuation_company_name_yields_the_documented_fallback() {
        let roadmap = roadmap_with_company("!!! --- ???");
        assert_eq!(deliverable_slug(&roadmap), FALLBACK_COMPANY_SLUG);
    }

    #[test]
    fn event_deserializes_inline_roadmap_locale_and_output_dir() {
        let json = serde_json::json!({
            "roadmap": {
                "situation": null,
                "candidates": [],
                "top_profiles": [],
                "recommendation": null,
                "authored_locale": "pt-BR",
            },
            "locale": "en-US",
            "output_dir": "/tmp/out",
        });
        let event: DeliverableRenderEventSchema = serde_json::from_value(json).unwrap();
        assert_eq!(event.locale, Locale::EnUs);
        assert_eq!(event.roadmap.authored_locale, Locale::PtBr);
        assert_eq!(event.output_dir, PathBuf::from("/tmp/out"));
    }

    #[test]
    fn omitting_locale_on_the_event_defaults_to_pt_br() {
        let json = serde_json::json!({
            "roadmap": {
                "situation": null,
                "candidates": [],
                "top_profiles": [],
                "recommendation": null,
                "authored_locale": "en-US",
            },
            "output_dir": "/tmp/out",
        });
        let event: DeliverableRenderEventSchema = serde_json::from_value(json).unwrap();
        assert_eq!(event.locale, Locale::PtBr);
    }

    #[test]
    fn locale_serde_round_trips_both_variants() {
        for locale in [Locale::PtBr, Locale::EnUs] {
            let value = serde_json::to_value(locale).unwrap();
            let round_tripped: Locale = serde_json::from_value(value).unwrap();
            assert_eq!(round_tripped, locale);
        }
    }
}
