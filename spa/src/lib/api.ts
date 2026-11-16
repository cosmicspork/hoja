export type FieldType = 'string' | 'text' | 'tags' | 'list' | 'enum' | 'date' | 'bool' | 'ref';
export type Value = string | string[] | boolean;
export type Fields = Record<string, Value>;

export interface FieldConfig {
  type: FieldType;
  title?: string;
  help?: string;
  required: boolean;
  person_only: boolean;
  options?: string[];
  ref_kind?: string;
}
export interface KindConfig { title?: string; label?: string; agent_direct?: boolean; fields: Record<string, FieldConfig> }
export interface SectionConfig { name: string; title?: string; /** Select records of this other type. */ type?: string; kind: string; where?: Record<string, string>; group_by?: string; item?: string }
export interface TypeConfig {
  title?: string;
  first_draft: 'human' | 'agent';
  agent_direct: boolean;
  template?: string;
  filename: string;
  exports: Format[];
  kinds: Record<string, KindConfig>;
  document: { fields: Record<string, FieldConfig>; sections: SectionConfig[] };
}
export type Format = 'pdf' | 'md' | 'html';
export type TypeState = 'accepted' | 'new' | 'changed' | 'missing';
export interface TypeStatus {
  name: string;
  state: TypeState;
  file_hash: string | null;
  accepted_hash: string | null;
  accepted_at: string | null;
  error: string | null;
  config: TypeConfig | null;
}
export interface TypeChange { name: string; file_hash: string | null; file_body: string | null; accepted_body: string | null; error: string | null }
export interface TemplateStatus {
  path: string;
  state: TypeState;
  file_hash: string | null;
  accepted_hash: string | null;
  accepted_at: string | null;
  error: string | null;
  used_by: string[];
}
export interface TemplateChange {
  path: string;
  file_hash: string | null;
  file_text: string | null;
  accepted_text: string | null;
  binary: boolean;
  file_size: number | null;
  accepted_size: number | null;
  error: string | null;
  used_by: string[];
}
export interface AppState {
  owner: string | null;
  workspace: string;
  types: TypeStatus[];
  types_error: string | null;
  templates: TemplateStatus[];
  templates_error: string | null;
  pending_approvals: number;
}

export type Author = 'person' | 'agent' | 'agent_accepted';
export interface Attribution { author: Author; agent?: string }
export interface HojaRecord {
  id: string;
  type: string;
  kind: string;
  fields: Fields;
  reviewed: boolean;
  reviewed_at: string | null;
  version: number;
  created_at: string;
  updated_at: string;
  deleted: boolean;
  created_by: Attribution | null;
  updated_by: Attribution | null;
}
export interface Revision {
  version: number;
  author: Author;
  agent: string | null;
  approval_id: string | null;
  edited: boolean;
  change: string;
  snapshot: { fields?: Fields; title?: string; reviewed?: boolean };
  created_at: string;
}
export interface Section { name: string; record_ids: string[] }
export interface HojaDocument {
  id: string;
  type: string;
  title: string;
  fields: Fields;
  sections: Section[];
  approved: boolean;
  approved_at: string | null;
  version: number;
  created_at: string;
  updated_at: string;
  created_by: Attribution | null;
  updated_by: Attribution | null;
}
export type Origin = 'person' | 'reviewed' | 'accepted' | 'agent' | 'layout';
export interface Span { text: string; origin: Origin; changed?: boolean; removed?: boolean; agent?: string; version?: number }
export interface FieldView { name: string; title: string; long: boolean; spans: Span[] }
export interface ItemView { id: string; type: string; kind: string; reviewed: boolean; added: boolean; label: Span[]; fields: FieldView[] }
export interface Provenance {
  baseline: boolean;
  title: Span[];
  fields: FieldView[];
  sections: { name: string; title: string; groups: { heading: Span[] | null; items: ItemView[] }[]; removed: string[] }[];
  summary: { agent_words: number; accepted_words: number; changed_words: number; removed_words: number };
}
export interface LintFinding { level: 'error' | 'warn'; message: string; record_id: string | null; field: string | null }
export interface DocumentView {
  document: HojaDocument;
  /** What approving this view approves; the server refuses it if anything changed since. */
  content_hash: string;
  lints: LintFinding[];
  blocking: number;
  exports: { format: Format; filename: string }[];
  has_template: boolean;
  revisions: Revision[];
  provenance: Provenance;
}
export type Action = 'record.create' | 'record.update' | 'document.create' | 'document.update' | 'document.select';
export interface Approval {
  id: string;
  action: Action;
  type: string;
  target_id: string | null;
  base_version: number | null;
  payload: { kind?: string; title?: string; fields?: Fields; section?: string; record_ids?: string[] };
  agent: string;
  note: string;
  state: 'pending' | 'accepted' | 'rejected' | 'changes_requested';
  reason: string | null;
  edited: boolean;
  result: unknown;
  created_at: string;
  decided_at: string | null;
  stale: boolean;
  current: null | { kind?: string; title?: string; fields?: Fields; sections?: Section[]; version?: number; deleted?: boolean };
}
export type Outcome<T> = { state: 'applied'; result: T } | { state: 'awaiting_person'; approval_id: string };
export type Patch = Record<string, Value | null>;

export class ApiError extends Error {
  constructor(public readonly code: string, message: string, public readonly status: number) { super(message); }
}

async function send(path: string, init: RequestInit = {}): Promise<Response> {
  const response = await fetch(path, {
    ...init,
    headers: { ...(init.body ? { 'Content-Type': 'application/json' } : {}), ...init.headers }
  });
  if (!response.ok) {
    let code = 'internal_error';
    let message = `Request failed (${response.status})`;
    try {
      const body = await response.json() as { error?: string; message?: string };
      code = body.error ?? code;
      message = body.message ?? message;
    } catch { /* retain safe status text */ }
    throw new ApiError(code, message, response.status);
  }
  return response;
}

async function request<T>(path: string, init: RequestInit = {}): Promise<T> {
  return (await send(path, init)).json() as Promise<T>;
}

// The person's writes are always applied; an outcome is only a card for agents.
async function applied<T>(path: string, init: RequestInit): Promise<T> {
  const outcome = await request<Outcome<T>>(path, init);
  if (outcome.state !== 'applied') throw new ApiError('internal_error', 'Expected the change to apply.', 500);
  return outcome.result;
}

const id = encodeURIComponent;
const json = (method: string, body: unknown): RequestInit => ({ method, body: JSON.stringify(body) });
const query = (params: Record<string, string | undefined>) => {
  const search = new URLSearchParams(Object.entries(params).filter(([, v]) => v) as [string, string][]).toString();
  return search ? `?${search}` : '';
};

export const api = {
  state: () => request<AppState>('/api/state'),
  typeChange: (name: string) => request<TypeChange>(`/api/types/${id(name)}/change`),
  acceptType: (name: string, hash: string) => request<unknown>(`/api/types/${id(name)}/accept`, json('POST', { hash })),
  templateChange: (path: string) => request<TemplateChange>(`/api/templates/change${query({ path })}`),
  acceptTemplate: (path: string, hash: string) => request<unknown>('/api/templates/accept', json('POST', { path, hash })),
  templatePreview: async (path: string, hash: string, type: string, document?: string) =>
    (await send('/api/templates/preview', json('POST', { path, hash, type, ...(document ? { document } : {}) }))).blob(),

  records: (type?: string, kind?: string) => request<HojaRecord[]>(`/api/records${query({ type, kind })}`),
  record: (rid: string) => request<{ record: HojaRecord; revisions: Revision[] }>(`/api/records/${id(rid)}`),
  createRecord: (type: string, kind: string, fields: Patch) => applied<HojaRecord>('/api/records', json('POST', { type, kind, fields })),
  updateRecord: (rid: string, version: number, fields: Patch) => applied<HojaRecord>(`/api/records/${id(rid)}`, json('PUT', { version, fields })),
  setReviewed: (rid: string, version: number, reviewed: boolean) => request<HojaRecord>(`/api/records/${id(rid)}/reviewed`, json('POST', { version, reviewed })),
  deleteRecord: (rid: string, version: number) => request<unknown>(`/api/records/${id(rid)}/delete`, json('POST', { version })),

  documents: (type?: string) => request<HojaDocument[]>(`/api/documents${query({ type })}`),
  document: (did: string) => request<DocumentView>(`/api/documents/${id(did)}`),
  createDocument: (type: string, title: string, fields: Patch) => applied<HojaDocument>('/api/documents', json('POST', { type, title, fields })),
  updateDocument: (did: string, version: number, title: string, fields: Patch) => applied<HojaDocument>(`/api/documents/${id(did)}`, json('PUT', { version, title, fields })),
  select: (did: string, section: string, version: number, record_ids: string[]) =>
    applied<HojaDocument>(`/api/documents/${id(did)}/sections/${id(section)}`, json('PUT', { version, record_ids })),
  approve: (did: string, version: number, hash: string) => request<HojaDocument>(`/api/documents/${id(did)}/approve`, json('POST', { version, hash })),
  preview: async (did: string) => (await send(`/api/documents/${id(did)}/preview.pdf`)).blob(),
  exportDocument: (did: string, format: Format) => request<{ path: string; filename: string; approved: boolean }>(`/api/documents/${id(did)}/export`, json('POST', { format })),

  markdown: (text: string) => request<{ html: string }>('/api/markdown', json('POST', { text })),

  approvals: (state?: string) => request<Approval[]>(`/api/approvals${query({ state })}`),
  decide: (aid: string, decision: 'accept' | 'reject' | 'request_changes', reason: string | null, payload?: Approval['payload']) =>
    request<Approval>(`/api/approvals/${id(aid)}/decide`, json('POST', { decision, reason, ...(payload ? { payload } : {}) }))
};

export function message(error: unknown, fallback: string): string {
  return error instanceof Error ? error.message : fallback;
}
