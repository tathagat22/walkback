// Outlook / Microsoft 365 hold-and-release via the Microsoft Graph API. Same
// model as Gmail: a draft message lives in the user's mailbox and has gone
// nowhere; /send delivers it, DELETE is a true unsend. Base URL overridable for
// tests; the bearer token is passed in (undo holds no Microsoft credentials).

import type { EmailDraft, EmailProvider } from "./email-provider.js";

export class OutlookClient implements EmailProvider {
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
      throw new Error(`Graph API ${method} ${path} → HTTP ${res.status} ${detail}`.trim());
    }
    return res.status === 204 ? {} : res.json();
  }

  async createDraft({ to, subject, body }: EmailDraft): Promise<{ id: string }> {
    const r = await this.request("POST", "/me/messages", {
      subject,
      body: { contentType: "Text", content: body },
      toRecipients: [{ emailAddress: { address: to } }],
    });
    return { id: String(r.id) };
  }

  async sendDraft(id: string): Promise<void> {
    await this.request("POST", `/me/messages/${id}/send`);
  }

  async deleteDraft(id: string): Promise<void> {
    await this.request("DELETE", `/me/messages/${id}`);
  }
}
