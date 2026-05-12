swagger: "2.0"
info:
  title: "Jira Webhook API"
  description: "API Gateway for Jira Webhooks"
  version: "1.0.0"
schemes:
  - "https"
paths:
  /:
    post:
      summary: "Jira Webhook Endpoint"
      operationId: "jiraWebhook"
      x-google-backend:
        address: "${cloud_run_url}"
      security:
        - api_key: []
      responses:
        '200':
          description: "A successful response"
securityDefinitions:
  api_key:
    type: "apiKey"
    name: "key"
    in: "query"