// Static interface and transport contracts; analytical work belongs to Rust.
// Rust prepares chart reports; WebAssembly scores only explicitly submitted profile interests.
export type MetricField = 'age_days' | 'length_bytes' | 'creator_prior_edits_main' | 'creator_prior_edits_other' |
  'creator_prior_articles' | 'creator_tenure_days' | 'creator_articles_created_total' | 'creator_orphans_current' |
  'creator_continuity_months_before_creation' | 'creator_prior_edits_main_history' |
  'creator_prior_edits_other_history' | 'quality_score';
export const ARTICLE_INDEX_FIELDS=['page_id','title','orphan','category','topic_status','topic_labels','length_bytes','created_at','in_degree'] as const;
export type PageObservation = Partial<Record<MetricField, number | null>> & {
  page_id: number; title: string; orphan: boolean; language?: string;
  category?: string | null; category_status?: string; revision_id?: number | null;
  created_at?: string | null; page_created_at?: string | null;
  in_degree?: number | null; out_degree?: number | null;
  creator_key?: string | null; creator_account_type?: string | null;
  history_creator_key?: string | null;
  creator_is_bot_now?: boolean | null; creator_languages_created?: string[] | null;
  creator_groups_at_creation_history?: string[] | null;
  quality_status?: string | null;
  topic_status?: string | null; topic_labels?: string[] | null; origin?: string | null;
};
export interface CreatorMetadata {
  status: string; language?: string; languages_scanned?: string[];
  started_at?: string; finished_at?: string; collected_languages?: number;
}
export interface HistoryMetadata { status: string; language?: string; matched_pages?: number; }
export interface DatasetMetadata {
  language: string; complete?: boolean; demo?: boolean; kind?: string; age_rule?: string; measures_rule?: string;
  started_at: string; finished_at: string; languages?: string[]; creators?: CreatorMetadata;
  history?: HistoryMetadata; comparison_coverage?: 'complete' | 'partial';
  topics?: {status?: string};
  biographies?: unknown;
}
export interface ReferenceEdge {
  language: string; source_page_id?: number | null; source_title: string;
  target_page_id?: number | null; target_title: string;
}
export interface Candidate {
  category?: string | null;
  page_id: number; title: string; out_degree?: number | null;
  dead_end?: boolean | null; evidence?: ReferenceEdge[];
}
export interface PromptCandidate extends Candidate { references: ReferenceEdge[]; }
export interface CandidateRecord {
  candidate_count?: number | null; status?: string; candidates?: Candidate[];
  candidate_ids?: number[];
}
export interface CardTransport { directory: string; identity: string; metadata: DatasetMetadata; census_page_count: number; }
export interface ConnectionMetadata { format: string; metadata: DatasetMetadata; census_page_count: number; connection_identity: string; }
export interface Dataset {
  metadata: DatasetMetadata; pages: PageObservation[];
  format?: string; census_page_count?: number;
  link_candidates?: Record<string, CandidateRecord>; biographies?: unknown;
  card_transports?: Record<string, CardTransport>;
}
export interface ManifestEntry {
  storage?: 'relational_census_v1';
  language: string; file: string; date: string; published_at?: string; kind?: string; demo?: boolean;
  summary_file: string;
  cards_dir?: string; connections_dir?: string; connections_metadata_file?: string; profile_topics_dir?: string;
}
export interface Provider { name: string; url: string | null; }
export interface PromptSource { id: string; language: string; title: string; page_id?: number | null; }
export interface PromptDocument {
  page_id: number; title: string; revision_id: number; timestamp: string;
  sha1: string; utf8_bytes: number; wikitext: string; retrieved_at: string;
}
export type ReviewStatus = 'fixed' | 'not_fixed' | 'bad_link';
export interface EditorialReview {
  language: string; source_id: number; target_id: number; status: ReviewStatus; version: number; updated_at: string; reviewer?: string;
}
export interface ReviewContext {
  dataset: string; language: string; target_id: number; target_title: string;
  sources: {page_id: number; title: string}[];
}
export interface PromptRequest { prompt: string; provider: Provider; sources: PromptSource[]; outputRules: string; review?: ReviewContext; }
export interface CreatorRow {
  percent: number | null;
  key: string; type?: string | null; articles?: number | null;
  total: number; orphans: number; languages: string[];
}
export interface FeatureGroup { count: number; known: number; total: number; }
export interface FeatureRow { label: string; groups: FeatureGroup[]; total: number; orphans: number; known: number; percent: number | null; }
export interface FeatureSummary { totals: number[]; rows: FeatureRow[]; }
export interface DashboardModule {
  default: () => Promise<unknown>;
  calculate: (kind: string, input: string) => string;
}
export interface CategoryReportRow { label: string; total: number; orphans: number; percent: number | null; }
export interface ProfilesReport {
  topics: FeatureSummary; macrothemes: FeatureSummary; features: FeatureSummary;
  thresholds: Record<string, [number | null, number | null, number | null]>;
}
export interface NumericGroup {
  total: number; known: number; missing: number; median: number | null;
  counts: number[]; percentages: (number | null)[]; cumulative: [number, number][];
}
export interface NumericReport {
  known: number; available: boolean; groups: NumericGroup[]; count_max: number;
  bin_edges: number[]; ticks: number[]; cumulative_ticks: number[];
  month_histogram?: {rule: 'months_0_12_overflow_v1'; through: 12; count_max: number; groups: {counts: number[]; percentages: (number | null)[]}[]};
}
export interface OriginsReport { origins: CategoryReportRow[]; bots: CategoryReportRow[]; known_bots: number; }
export interface OverviewReport {
  total: number; orphans: number; percent: number | null; dead_ends: number;
  attributed_orphans: number; missing_creators: number; distinct_creators: number;
}
export interface DashboardSummaryData {
  format: 'dashboard_summary_v1'; measures_rule: string; metadata: DatasetMetadata; census_page_count: number;

  reports: {
    overview: OverviewReport; categories: CategoryReportRow[]; profiles: ProfilesReport;
    numeric: Record<string, NumericReport>; origins: OriginsReport;
    creator_groups: CategoryReportRow[];
  };
}
export interface RecentEdit { language: string; page_id: number; namespace: number; timestamp: string; topic_labels: string[] | null; }
export interface InterestReport {
  total_edits: number; known_topic_edits: number;
  scores: {key: string; topic: number; language: number}[];
}
export interface GlobalWiki { wiki: string; url: string; id: number; editcount?: number; }
export interface GlobalUser { id: number; name: string; merged: GlobalWiki[]; missing?: boolean; }
export interface UserContribution { userid: number; pageid: number; ns: number; timestamp: string; }

export interface UIElements {
  'age': HTMLElement;
  'age-note': HTMLElement;
  'article-categories': HTMLElement;
  'article-category': HTMLInputElement;
  'article-category-control': HTMLElement;
  'article-macro': HTMLSelectElement;
  'article-macro-control': HTMLElement;
  'article-category-heading': HTMLElement;
  'article-category-label': HTMLElement;
  'article-classification': HTMLSelectElement;
  'article-group': HTMLSelectElement;
  'article-info': HTMLElement;
  'article-more': HTMLButtonElement;
  'article-previous': HTMLButtonElement;
  'article-pages': HTMLElement;
  'article-rows': HTMLElement;
  'article-search': HTMLInputElement;
  'article-sort': HTMLSelectElement;
  'articles-link': HTMLAnchorElement;
  'candidate-cards': HTMLElement;
  'candidate-categories': HTMLElement;
  'candidate-category': HTMLInputElement;
  'candidate-heading': HTMLElement;
  'candidate-info': HTMLElement;
  'candidate-more': HTMLButtonElement;
  'candidate-previous': HTMLButtonElement;
  'candidate-pages': HTMLElement;
  'candidate-search': HTMLInputElement;
  'candidate-section': HTMLElement;
  'candidate-sort': HTMLSelectElement;
  'candidate-hidden': HTMLButtonElement;
  'candidate-hidden-control': HTMLElement;
  'candidate-hidden-options': HTMLElement;
  'candidate-show-disambiguation': HTMLInputElement;
  'candidate-show-redirects': HTMLInputElement;
  'candidate-show-without-connections': HTMLInputElement;
  'candidate-show-names': HTMLInputElement;
  'candidate-show-indexes': HTMLInputElement;
  'candidate-show-maintenance': HTMLInputElement;
  'candidate-hide-link-disambiguation': HTMLInputElement;
  'candidate-hide-link-names': HTMLInputElement;
  'candidate-hide-link-redirects': HTMLInputElement;
  'auth-menu': HTMLDetailsElement;
  'auth-toggle': HTMLElement;
  'auth-login': HTMLButtonElement;
  'auth-user': HTMLAnchorElement;
  'auth-logout': HTMLButtonElement;
  'auth-status': HTMLElement;
  'profile-sort-option': HTMLOptionElement;
  'profile-username': HTMLInputElement;
  'profile-apply': HTMLButtonElement;
  'profile-status': HTMLElement;
  'categories': HTMLElement;
  'category-metric': HTMLSelectElement;
  'chart-type': HTMLSelectElement;
  'connection-candidates': HTMLElement;
  'connection-copy-prompt': HTMLButtonElement;
  'connection-info': HTMLElement;
  'connection-more': HTMLButtonElement;
  'connection-target': HTMLElement;
  'connection-title': HTMLElement;
  'creator-bots': HTMLElement;
  'creator-bots-note': HTMLElement;
  'creator-groups': HTMLElement;
  'creator-groups-note': HTMLElement;
  'history-section': HTMLElement;
  'creator_continuity_months_before_creation-chart': HTMLElement;
  'creator_continuity_months_before_creation-note': HTMLElement;
  'creator-info': HTMLElement;
  'creator-more': HTMLButtonElement;
  'creator-previous': HTMLButtonElement;
  'creator-pages': HTMLElement;
  'creator-rows': HTMLElement;
  'creator-search': HTMLInputElement;
  'creator-section': HTMLElement;
  'creator-sort': HTMLSelectElement;
  'creator_articles_created_total-chart': HTMLElement;
  'creator_articles_created_total-note': HTMLElement;
  'creator_orphans_current-chart': HTMLElement;
  'creator_orphans_current-note': HTMLElement;
  'creator_prior_articles-chart': HTMLElement;
  'creator_prior_articles-note': HTMLElement;
  'creator_prior_edits_main-chart': HTMLElement;
  'creator_prior_edits_main-note': HTMLElement;
  'creator_prior_edits_other-chart': HTMLElement;
  'creator_prior_edits_other-note': HTMLElement;
  'creator_tenure_days-chart': HTMLElement;
  'creator_tenure_days-note': HTMLElement;
  'creators-empty': HTMLElement;
  'creators-link': HTMLAnchorElement;
  'dataset': HTMLSelectElement;
  'dataset-date': HTMLSelectElement;
  'download': HTMLAnchorElement;
  'deorphanize-link': HTMLAnchorElement;
  'features-paper': HTMLElement;
  'length': HTMLElement;
  'llm-prompt': HTMLDialogElement;
  'llm-prompt-close': HTMLButtonElement;
  'llm-prompt-finished': HTMLButtonElement;
  'review-dialog': HTMLDialogElement;
  'review-rows': HTMLElement;
  'review-dialog-status': HTMLElement;
  'review-close': HTMLButtonElement;
  'review-service-status': HTMLElement;
  'llm-prompt-copy': HTMLButtonElement;
  'llm-prompt-debug': HTMLInputElement;
  'llm-prompt-open': HTMLAnchorElement;
  'llm-prompt-status': HTMLElement;
  'llm-prompt-text': HTMLTextAreaElement;
  'llm-prompt-title': HTMLElement;
  'llm-provider': HTMLSelectElement;
  'metrics': HTMLElement;
  'origin-section': HTMLElement;
  'origins': HTMLElement;
  'status': HTMLElement;
  'theme-dark-icon': HTMLElement;
  'theme-light-icon': HTMLElement;
  'theme-toggle': HTMLButtonElement;
  'topics-full': HTMLElement;
  'topics-full-metric': HTMLSelectElement;
  'topics-paper': HTMLElement;
  'topics-paper-metric': HTMLSelectElement;
  'ui-language-toggle': HTMLButtonElement;
}
