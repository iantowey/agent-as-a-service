import { tool } from "@opencode-ai/plugin"

export default tool({
  description:
    "Send a notification message to the user's Microsoft Teams channel. Use this to notify the user when tasks are completed.",
  args: {
    message: tool.schema
      .string()
      .describe("The notification message to send to Teams"),
  },
  async execute(args) {
    const url = process.env.TEAMS_WEBHOOK_URL
    if (!url) {
      return "Error: TEAMS_WEBHOOK_URL environment variable is not set"
    }
    // Power Automate webhook templates expect a simple object with
    // "title" and "text" keys. The template maps these into its
    // built-in Adaptive Card.
    const payload = {
      title: "OpenCode",
      text: args.message,
      attachments: [],
    }
    const response = await fetch(url, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(payload),
    })
    if (response.ok) {
      return `Teams notification sent: ${args.message}`
    }
    const responseBody = await response.text()
    return `Failed to send Teams notification: ${response.status} ${response.statusText} - ${responseBody}`
  },
})
