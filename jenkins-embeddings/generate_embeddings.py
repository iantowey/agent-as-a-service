import os
import json
import faiss
import numpy as np
from sentence_transformers import SentenceTransformer
from google.cloud import storage

# --- Configuration (Passed from Jenkins Env Vars) ---
REPO_DIR = os.environ.get("WORKSPACE", ".")
PROJECT_NAME = os.environ.get("PROJECT_NAME") # e.g. jvm-func-pantry
BRANCH_NAME = os.environ.get("BRANCH_NAME", "master")
GCS_BUCKET_NAME = os.environ.get("GCS_BUCKET_NAME", "my-dedev-gcs-bucket")
GCS_ROOT_PATH = os.environ.get("GCS_ROOT_PATH", "embeddings")

# --- Constants ---
CHUNK_SIZE = 1500 # Characters per chunk
# We use a fast, lightweight open-source model from Hugging Face
EMBEDDING_MODEL_NAME = "all-MiniLM-L6-v2"
EMBEDDING_DIMENSIONS = 384 # Dimension size for all-MiniLM-L6-v2
IGNORE_DIRS = {'.git', 'target', 'node_modules', 'venv', '.idea', 'build', 'dist'}

def get_files_to_index(root_dir):
    files_to_index = []
    for dirpath, dirnames, filenames in os.walk(root_dir):
        # Mutate dirnames in place to skip ignored directories
        dirnames[:] = [d for d in dirnames if d not in IGNORE_DIRS]
        for file in filenames:
            # Skip obvious binary/media files
            if not file.endswith(('.jar', '.class', '.png', '.jpg', '.pyc', '.zip', '.tar.gz')):
                files_to_index.append(os.path.join(dirpath, file))
    return files_to_index

def chunk_text(text, chunk_size):
    """Splits text into chunks of approximately chunk_size characters."""
    return [text[i:i + chunk_size] for i in range(0, len(text), chunk_size)]

def generate_embeddings():
    print(f"Starting embedding generation for {PROJECT_NAME}/{BRANCH_NAME}...")
    
    # Load the local Hugging Face model
    print(f"Loading local embedding model '{EMBEDDING_MODEL_NAME}'...")
    model = SentenceTransformer(EMBEDDING_MODEL_NAME)
    
    files = get_files_to_index(REPO_DIR)
    print(f"Found {len(files)} files to index.")
    
    documents = []
    metadata = []
    
    for file_path in files:
        try:
            with open(file_path, 'r', encoding='utf-8') as f:
                content = f.read()
                
            # Create chunks for large files
            chunks = chunk_text(content, CHUNK_SIZE)
            for i, chunk in enumerate(chunks):
                # Prepend the file path to the chunk so the LLM gets strong context
                doc_text = f"File: {file_path} (Part {i+1})\n\n{chunk}"
                documents.append(doc_text)
                metadata.append({
                    "file_path": file_path,
                    "chunk_index": i,
                    "content": doc_text
                })
        except Exception as e:
            # Skip files that aren't readable text (e.g. hidden binaries)
            pass
            
    if not documents:
        print("No readable documents found. Exiting.")
        return
        
    print(f"Generated {len(documents)} chunks. Computing local embeddings...")
    
    # SentenceTransformer handles batching automatically for performance
    embeddings = model.encode(documents, show_progress_bar=True)
    
    # Convert to numpy array for FAISS (SentenceTransformer already returns numpy arrays)
    embeddings_np = np.array(embeddings).astype('float32')
    
    # --- Build FAISS Index ---
    print("Building FAISS index...")
    index = faiss.IndexFlatL2(EMBEDDING_DIMENSIONS)
    index.add(embeddings_np)
    
    # Save locally first
    faiss.write_index(index, "faiss.index")
    with open("metadata.json", "w") as f:
        json.dump(metadata, f)
        
    # --- Upload to GCS ---
    print(f"Uploading to GCS: gs://{GCS_BUCKET_NAME}/{GCS_ROOT_PATH}/{PROJECT_NAME}/{BRANCH_NAME}/")
    storage_client = storage.Client()
    bucket = storage_client.bucket(GCS_BUCKET_NAME)
    
    gcs_prefix = f"{GCS_ROOT_PATH}/{PROJECT_NAME}/{BRANCH_NAME}"
    
    blob_index = bucket.blob(f"{gcs_prefix}/faiss.index")
    blob_index.upload_from_filename("faiss.index")
    
    blob_meta = bucket.blob(f"{gcs_prefix}/metadata.json")
    blob_meta.upload_from_filename("metadata.json")
    
    print("Upload complete!")

if __name__ == "__main__":
    generate_embeddings()