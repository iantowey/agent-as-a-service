#!/bin/bash
# Generates a K8s secret manifest from the downloaded JSON key
cat << YAML > webhook-ingester/secret.yaml
apiVersion: v1
kind: Secret
metadata:
  name: webhook-ingester-gcp-sa
  namespace: dedev
type: Opaque
data:
  credentials.json: $(base64 -w 0 webhook-ingester/terraform/webhook-ingester-sa-key.json)
YAML
