#!/bin/bash
set -e

# ==============================================================================
# Opencode Bootstrap Worker Entrypoint
# Description: Clones the target repo, boots its custom docker-compose.dev.yml
# sandbox, and runs opencode against that environment.
# ==============================================================================

echo "Starting Opencode Bootstrap Worker..."

if [ -z "$EVENT_SOURCE" ] || [ -z "$EVENT_TYPE" ]; then
    echo "ERROR: EVENT_SOURCE and EVENT_TYPE must be set."
    exit 1
fi

echo "Waiting for Docker daemon (DinD sidecar)..."
timeout 30 bash -c 'until docker info >/dev/null 2>&1; do sleep 1; done'
if [ $? -ne 0 ]; then
    echo "ERROR: Docker daemon did not become ready within 30 seconds."
    exit 1
fi

echo "Configuring Git authentication..."
git config --global credential.helper store
echo "https://${BITBUCKET_TOKEN}:@git.overstock.com" > ~/.git-credentials
git config --global user.name "Ian Towey"
git config --global user.email "itowey@beyond.com"

if [ "$EVENT_SOURCE" == "bitbucket" ]; then
    if [ -z "$PROJECT_KEY" ] || [ -z "$REPO_SLUG" ] || [ -z "$BRANCH_NAME" ]; then
        echo "ERROR: Missing Bitbucket repository parameters."
        exit 1
    fi
    
    REPO_URL="https://itowey:${BITBUCKET_TOKEN}@git.overstock.com/scm/${PROJECT_KEY,,}/${REPO_SLUG}.git"
    echo "Cloning repository: https://git.overstock.com/scm/${PROJECT_KEY,,}/${REPO_SLUG}.git on branch: $BRANCH_NAME"
    
    git clone --branch "$BRANCH_NAME" "$REPO_URL" /workspace/repo
    cd /workspace/repo

    if [ "$EVENT_TYPE" == "pr_comment" ]; then
        AGENT_PROMPT="
        You are an autonomous engineering agent.
        
        A new comment has been posted on Pull Request #$PR_ID for the branch '$BRANCH_NAME'.
        The comment says:
        \"$COMMENT_TEXT\"
        
        Please perform the following:
        1. Analyze the comment and codebase. 
        2. Implement the requested changes.
        3. Test your changes using standard tools.
        4. Commit your changes and push them back to origin/$BRANCH_NAME.
        5. Reply to the PR comment using the bitbucket API to confirm the fix, prefixing your comment with '<<OPENCODE>> '.
        "
    else
        echo "ERROR: Unhandled Bitbucket event type: $EVENT_TYPE"
        exit 1
    fi

elif [ "$EVENT_SOURCE" == "jira" ]; then
    echo "Event is a Jira ticket update."

    # Step 1: Use the standard Jira REST API to resolve the issueKey (e.g. DE-3261) to the numeric issueId.
    # The Dev Status API strictly requires the numeric ID.
    echo "Resolving numeric Issue ID for Jira Key: $ISSUE_KEY..."
    ISSUE_DATA_JSON=$(curl -s -u "$JIRA_EMAIL:$JIRA_TOKEN" \
         -H "Content-Type: application/json" \
         "https://overstock.atlassian.net/rest/api/2/issue/${ISSUE_KEY}?fields=id")
         
    NUMERIC_ISSUE_ID=$(echo "$ISSUE_DATA_JSON" | jq -r '.id // empty')

    if [ -z "$NUMERIC_ISSUE_ID" ]; then
        echo "ERROR: Failed to resolve numeric Issue ID for $ISSUE_KEY. Response: $ISSUE_DATA_JSON"
    else
        # Step 2: Fetch branch details from the Jira Dev Status API using the numeric ID
        echo "Fetching branch details for Jira Issue ID: $NUMERIC_ISSUE_ID ($ISSUE_KEY)..."
        DEV_STATUS_JSON=$(curl -s -u "$JIRA_EMAIL:$JIRA_TOKEN" \
             -H "Content-Type: application/json" \
             "https://overstock.atlassian.net/rest/dev-status/1.0/issue/detail?issueId=${NUMERIC_ISSUE_ID}&applicationType=stash&dataType=branch")

        # Use jq to parse out the branch name and repo URL.
        BRANCH_NAME=$(echo "$DEV_STATUS_JSON" | jq -r '.detail[0].branches[0].name // empty')
        REPO_URL=$(echo "$DEV_STATUS_JSON" | jq -r '.detail[0].branches[0].repository.url // empty')
    fi

    if [ -n "$BRANCH_NAME" ] && [ -n "$REPO_URL" ]; then
        echo "Found branch $BRANCH_NAME for repository $REPO_URL linked to $ISSUE_KEY."
        echo "Cloning repository..."
        
        # Inject the bitbucket token into the REPO_URL for authentication
        # The Jira API returned a browse URL, we need to convert it to an SCM clone URL
        # from: https://git.overstock.com/projects/DEDEV/repos/salesforce-table-export/browse
        # to:   https://git.overstock.com/scm/dedev/salesforce-table-export.git
        PROJECT_KEY=$(echo "$REPO_URL" | sed -n 's|.*projects/\([^/]*\)/repos.*|\1|p')
        REPO_SLUG=$(echo "$REPO_URL" | sed -n 's|.*repos/\([^/]*\)/browse.*|\1|p')
        
        # URL encode the token because it contains a slash which breaks the Git HTTPS clone URL
        ENCODED_TOKEN=$(printf '%s' "$BITBUCKET_TOKEN" | jq -sRr @uri)
        CLONE_URL="https://itowey:${ENCODED_TOKEN}@git.overstock.com/scm/${PROJECT_KEY,,}/${REPO_SLUG}.git"
        
        git clone --branch "$BRANCH_NAME" "$CLONE_URL" /workspace/repo
        cd /workspace/repo

        AGENT_PROMPT="
        You are an autonomous engineering agent.
        
        A Jira ticket ($ISSUE_KEY) has been updated and a branch ($BRANCH_NAME) is linked.
        Summary: $SUMMARY
        Description: $DESCRIPTION
        
        Please perform the following steps carefully:
        1. Read the AGENTS.md file in the root of the repository to understand what the project is about and its specific conventions.
        2. Start the custom sandbox environment by running \`docker compose -f docker-compose.dev.yml up -d\`. The docker-compose file mounts the current repo into the container.
        3. Implement the requested changes to address the Jira ticket, strictly following the guidelines in AGENTS.md.
        4. Do all compiling, testing, and building of the code INSIDE the docker-in-docker container (e.g. using \`docker exec\`). Do NOT run build tools on the host.
        5. Once changes are verified, commit your changes and push them back to origin/$BRANCH_NAME.
        6. Tear down the sandbox using \`docker compose -f docker-compose.dev.yml down\`.
        7. Notify the user via the 'teams-notify' tool that the task is complete.
        "
    else
        echo "WARNING: Could not find branch details for Jira Issue $ISSUE_KEY. The agent will run without code context."
        AGENT_PROMPT="
        A Jira ticket ($ISSUE_KEY) has been updated.
        Summary: $SUMMARY
        Description: $DESCRIPTION
        
        Formulate a plan to address this ticket and notify the user via the 'teams-notify' tool. 
        No code repository is linked.
        "
        cd /workspace
    fi

else
    echo "ERROR: Unknown EVENT_SOURCE: $EVENT_SOURCE"
    exit 1
fi

echo "Executing Opencode..."

# Set standard API tokens for MCP servers
export JIRA_API_TOKEN="$JIRA_TOKEN"
export BITBUCKET_API_TOKEN="$BITBUCKET_TOKEN"

# Execute opencode using the standard Google Gemini provider (not Vertex)
# Pass the API key using the environment variable format Opencode expects
export GOOGLE_GENERATIVE_AI_API_KEY="$GEMINI_API_KEY"
opencode run --dangerously-skip-permissions --model google/gemini-3.1-pro-preview "$AGENT_PROMPT"

EXIT_CODE=$?
echo "Opencode finished with exit code $EXIT_CODE"

# Cleanup Custom Sandbox
if [ "$HAS_COMPOSE" = true ]; then
    echo "Tearing down repository sandbox..."
    docker compose -f docker-compose.dev.yml down -v || true
fi

exit $EXIT_CODE
