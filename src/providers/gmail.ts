// Gmail hold-and-release via the Gmail REST API. A draft sits in the user's own
// account and has gone nowhere; sending delivers it (no recall after), deleting
// it is a true unsend. The base URL is overridable so the flow can be tested
// against a mock server. undo holds no Google credentials — the token is passed in.

import type { EmailDraft, EmailProvider } from "./email-provider.js";

export class GmailClient implements EmailProvider {
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

  async createDraft(draft: EmailDraft): Promise<{ id: string }> {
    const raw = encodeMessage(draft);
    const r = await this.request("POST", "/users/me/drafts", { message: { raw } });
    return { id: String(r.id) };
  }

  async sendDraft(id: string): Promise<void> {
    await this.request("POST", "/users/me/drafts/send", { id });
  }

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
