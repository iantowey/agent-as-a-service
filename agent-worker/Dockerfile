# Bootstrap Agent Worker
# This image contains ONLY the tools needed to clone code and orchestrate docker.
# It relies on the target repository providing a 'docker-compose.dev.yml' and 'dev.Dockerfile'
# to define the actual build environment (Java, Node, Python, etc.).

FROM python:3.10-slim

ENV DEBIAN_FRONTEND=noninteractive

# 1. Install Git, Curl, jq, Node.js (for npx) and utilities
RUN apt-get update && apt-get install -y \
    git \
    curl \
    wget \
    jq \
    apt-transport-https \
    ca-certificates \
    gnupg \
    && curl -fsSL https://deb.nodesource.com/setup_18.x | bash - \
    && apt-get install -y nodejs \
    && curl https://packages.cloud.google.com/apt/doc/apt-key.gpg | gpg --dearmor -o /usr/share/keyrings/cloud.google.gpg \
    && echo "deb [signed-by=/usr/share/keyrings/cloud.google.gpg] https://packages.cloud.google.com/apt cloud-sdk main" | tee -a /etc/apt/sources.list.d/google-cloud-sdk.list \
    && apt-get update && apt-get install -y google-cloud-cli \
    && rm -rf /var/lib/apt/lists/*

# 2. Install Docker CLI & Compose plugin (To interact with the DinD sidecar)
RUN curl -fsSL https://download.docker.com/linux/debian/gpg | gpg --dearmor -o /usr/share/keyrings/docker-archive-keyring.gpg && \
    echo "deb [arch=$(dpkg --print-architecture) signed-by=/usr/share/keyrings/docker-archive-keyring.gpg] https://download.docker.com/linux/debian $(awk -F'=' '/^VERSION_CODENAME=/{print $2}' /etc/os-release) stable" | tee /etc/apt/sources.list.d/docker.list > /dev/null && \
    apt-get update && apt-get install -y docker-ce-cli docker-compose-plugin && \
    rm -rf /var/lib/apt/lists/*

# 3. Download Opencode CLI via npm
#RUN npm install -g @opencode-ai
RUN curl -fsSL https://opencode.ai/install | bash

# 4. Set up Workspace
RUN mkdir -p /workspace
WORKDIR /workspace

# 5. Set up Opencode Configuration
RUN mkdir -p /root/.config/opencode
COPY config/opencode.json /root/.config/opencode/opencode.json

# 6. Copy the Bootstrap Script
COPY entrypoint.sh /usr/local/bin/entrypoint.sh
RUN chmod +x /usr/local/bin/entrypoint.sh

ENTRYPOINT ["/usr/local/bin/entrypoint.sh"]
