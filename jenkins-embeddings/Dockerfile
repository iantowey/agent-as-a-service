# Use a lightweight Python base image
FROM python:3.10-slim

# Avoid apt prompts
ENV DEBIAN_FRONTEND=noninteractive

# Update system and install basic build tools (needed for numpy/faiss C++ extensions)
RUN apt-get update && apt-get install -y \
    build-essential \
    curl \
    && rm -rf /var/lib/apt/lists/*

# Install required Python libraries for embedding generation and GCS upload
# sentence-transformers is heavy (installs PyTorch), so this build might take a minute
RUN pip install --no-cache-dir \
    faiss-cpu==1.7.4 \
    numpy==1.26.4 \
    sentence-transformers==2.5.1 \
    google-cloud-storage==2.14.0

# Set a working directory
WORKDIR /app

COPY generate_embeddings.py .

# The entrypoint will simply be Python, allowing Jenkins to run the script via sh
ENTRYPOINT ["python3"]
