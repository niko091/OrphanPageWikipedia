use super::*;

#[derive(Deserialize)]
pub(super) struct InterestEdit {
    pub(super) language: String,
    pub(super) topic_labels: Option<Vec<String>>,
}

#[derive(Deserialize)]
pub(super) struct InterestCandidate {
    pub(super) key: String,
    pub(super) language: String,
    pub(super) topic_labels: Option<Vec<String>>,
}

#[derive(Deserialize)]
pub(super) struct InterestInput {
    pub(super) edits: Vec<InterestEdit>,
    pub(super) candidates: Vec<InterestCandidate>,
    pub(super) aggregate: bool,
}

#[derive(Serialize)]
pub(super) struct InterestScore {
    pub(super) key: String,
    pub(super) topic: f64,
    pub(super) language: f64,
}

#[derive(Serialize)]
pub(super) struct InterestReport {
    pub(super) total_edits: usize,
    pub(super) known_topic_edits: usize,
    pub(super) scores: Vec<InterestScore>,
}

pub(super) fn interest_scores(input: InterestInput) -> InterestReport {
    let total_edits = input.edits.len();
    let mut known_topic_edits = 0;
    let mut topics = BTreeMap::<String, f64>::new();
    let mut languages = BTreeMap::<String, usize>::new();
    for edit in &input.edits {
        *languages.entry(edit.language.clone()).or_default() += 1;
        if let Some(labels) = &edit.topic_labels {
            let unique: BTreeSet<_> = labels.iter().filter(|label| !label.is_empty()).collect();
            if !unique.is_empty() {
                known_topic_edits += 1;
                let share = 1.0 / unique.len() as f64;
                for label in unique {
                    *topics.entry(label.clone()).or_default() += share;
                }
            }
        }
    }
    let scores = input
        .candidates
        .into_iter()
        .map(|candidate| {
            let unique: BTreeSet<_> = candidate
                .topic_labels
                .as_ref()
                .into_iter()
                .flatten()
                .filter(|label| !label.is_empty())
                .collect();
            let topic = if known_topic_edits == 0 {
                0.0
            } else {
                unique
                    .into_iter()
                    .map(|label| topics.get(label).copied().unwrap_or(0.0))
                    .sum::<f64>()
                    / known_topic_edits as f64
            };
            let language = if input.aggregate && total_edits > 0 {
                languages.get(&candidate.language).copied().unwrap_or(0) as f64 / total_edits as f64
            } else {
                0.0
            };
            InterestScore {
                key: candidate.key,
                topic,
                language,
            }
        })
        .collect();
    InterestReport {
        total_edits,
        known_topic_edits,
        scores,
    }
}
