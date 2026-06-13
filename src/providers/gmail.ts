// Gmail hold-and-release client — the honest "true unsend" primitive.
//
// The only way an email can genuinely "never be sent" is if it hasn't left yet.
// So instead of sending immediately, we create a Gmail *draft*. A draft sits in
// the user's own account and has gone nowhere. From there:
//   - sendDraft()   actually delivers it (after this, it's gone — no recall)
//   - deleteDraft() removes it before it was ever sent (true unsend)
//
// This client speaks the Gmail REST API directly with a caller-supplied bearer
// token, so `undo` holds no Google credentials of its own. The base URL is
// overridable so the whole flow can be tested against a mock server.

export interface EmailDraft {
  to: string;
  subject: string;
  body: string;
}

export class GmailClient {
  constructor(
    private readonly baseUrl: string,
    private readonly token: string,
  ) {}

  private async request(method: string, path: string, body?: unknown): Promise<any> {
    const res = await fetch(`${this.baseUrl}${path}`, {
      method,
      headers: {
        authorization: `Bearer ${this.token}`,
        "content-type": "application/json",
      },
      body: body === undefined ? undefined : JSON.stringify(body),
    });
    if (!res.ok) {
      const detail = await res.text().catch(() => "");
      throw new Error(`Gmail API ${method} ${path} → HTTP ${res.status} ${detail}`.trim());
    }
    return res.status === 204 ? {} : res.json();
  }

  /** Create a draft. It is NOT sent — it has gone nowhere yet. */
  async createDraft(draft: EmailDraft): Promise<{ id: string }> {
    const raw = encodeMessage(draft);
    const r = await this.request("POST", "/users/me/drafts", { message: { raw } });
    return { id: String(r.id) };
  }

  /** Actually deliver a held draft. After this the email is out — no recall. */
  async sendDraft(id: string): Promise<{ id: string }> {
    const r = await this.request("POST", "/users/me/drafts/send", { id });
    return { id: String(r.id ?? id) };
  }

  /** Delete a held draft before it was ever sent — a true unsend. */
  async deleteDraft(id: string): Promise<void> {
    await this.request("DELETE", `/users/me/drafts/${id}`);
  }
}

/** Encode an email as the base64url RFC 822 message Gmail expects. */
export function encodeMessage({ to, subject, body }: EmailDraft): string {
  const mime =
    `To: ${to}\r\n` +
    `Subject: ${subject}\r\n` +
    `Content-Type: text/plain; charset="UTF-8"\r\n` +
    `\r\n` +
    body;
  return Buffer.from(mime, "utf8").toString("base64url");
}
