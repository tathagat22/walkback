// The hold-and-release primitive, provider-agnostic. Any email backend that can
// create a draft, send it, and delete it supports a true "unsend" (cancel the
// draft before it's ever sent). Gmail and Outlook both can.

export interface EmailDraft {
  to: string;
  subject: string;
  body: string;
}

export interface EmailProvider {
  /** Create a draft. It is NOT sent — it has gone nowhere yet. */
  createDraft(draft: EmailDraft): Promise<{ id: string }>;
  /** Deliver a held draft. After this the email is out — no recall. */
  sendDraft(id: string): Promise<void>;
  /** Delete a held draft before it was ever sent — a true unsend. */
  deleteDraft(id: string): Promise<void>;
}
