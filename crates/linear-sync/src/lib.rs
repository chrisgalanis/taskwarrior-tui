use anyhow::{Context, Result};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

pub mod config;

const LINEAR_API: &str = "https://api.linear.app/graphql";

pub struct LinearClient {
    token: String,
    http: Client,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct LinearUser {
    pub id: String,
    pub name: String,
    pub email: String,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct WorkflowState {
    pub id: String,
    pub name: String,
    #[serde(rename = "type")]
    pub kind: String,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct WorkflowStateConnection {
    pub nodes: Vec<WorkflowState>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct LinearTeam {
    pub id: String,
    pub name: String,
    pub states: WorkflowStateConnection,
}

#[derive(Debug, Deserialize, Clone)]
pub struct LinearIssue {
    pub id: String,
    pub identifier: String,
    pub url: String,
}

#[derive(Debug, Deserialize, Clone)]
pub struct LinearComment {
    pub id: String,
}

impl LinearClient {
    pub fn new(token: impl Into<String>) -> Self {
        Self {
            token: token.into(),
            http: Client::new(),
        }
    }

    async fn graphql(&self, query: &str, variables: Value) -> Result<Value> {
        let res = self
            .http
            .post(LINEAR_API)
            .header("Authorization", &self.token)
            .header("Content-Type", "application/json")
            .json(&json!({ "query": query, "variables": variables }))
            .send()
            .await
            .context("HTTP request to Linear API failed")?;

        let body: Value = res.json().await.context("Failed to decode Linear response")?;

        if let Some(errors) = body.get("errors") {
            anyhow::bail!("Linear API error: {}", errors);
        }

        Ok(body["data"].clone())
    }

    pub async fn get_viewer(&self) -> Result<LinearUser> {
        let data = self.graphql("{ viewer { id name email } }", json!({})).await?;
        serde_json::from_value(data["viewer"].clone()).context("Failed to parse viewer")
    }

    pub async fn find_user_by_email(&self, email: &str) -> Result<Option<LinearUser>> {
        const Q: &str = r#"
            query($email: String!) {
                users(filter: { email: { eq: $email } }) {
                    nodes { id name email }
                }
            }
        "#;
        let data = self.graphql(Q, json!({ "email": email })).await?;
        let nodes: Vec<LinearUser> =
            serde_json::from_value(data["users"]["nodes"].clone()).context("Failed to parse users")?;
        Ok(nodes.into_iter().next())
    }

    pub async fn get_teams(&self) -> Result<Vec<LinearTeam>> {
        const Q: &str = r#"
            { teams { nodes { id name states { nodes { id name type } } } } }
        "#;
        let data = self.graphql(Q, json!({})).await?;
        serde_json::from_value(data["teams"]["nodes"].clone()).context("Failed to parse teams")
    }

    /// Create a new issue and return it.
    pub async fn create_issue(
        &self,
        title: &str,
        description: Option<&str>,
        team_id: &str,
        assignee_id: &str,
    ) -> Result<LinearIssue> {
        const Q: &str = r#"
            mutation($title: String!, $teamId: String!, $assigneeId: String, $description: String) {
                issueCreate(input: {
                    title: $title
                    teamId: $teamId
                    assigneeId: $assigneeId
                    description: $description
                }) {
                    success
                    issue { id identifier url }
                }
            }
        "#;
        let data = self
            .graphql(
                Q,
                json!({
                    "title": title,
                    "teamId": team_id,
                    "assigneeId": assignee_id,
                    "description": description,
                }),
            )
            .await?;
        serde_json::from_value(data["issueCreate"]["issue"].clone())
            .context("Failed to parse created issue")
    }

    /// Update the title and/or description of an existing issue.
    pub async fn update_issue(
        &self,
        issue_id: &str,
        title: Option<&str>,
        description: Option<&str>,
    ) -> Result<()> {
        const Q: &str = r#"
            mutation($id: String!, $title: String, $description: String) {
                issueUpdate(id: $id, input: { title: $title, description: $description }) {
                    success
                }
            }
        "#;
        self.graphql(
            Q,
            json!({ "id": issue_id, "title": title, "description": description }),
        )
        .await?;
        Ok(())
    }

    /// Move an issue to a specific workflow state (e.g. "completed").
    pub async fn set_issue_state(&self, issue_id: &str, state_id: &str) -> Result<()> {
        const Q: &str = r#"
            mutation($id: String!, $stateId: String!) {
                issueUpdate(id: $id, input: { stateId: $stateId }) {
                    success
                }
            }
        "#;
        let data = self
            .graphql(Q, json!({ "id": issue_id, "stateId": state_id }))
            .await?;
        if !data["issueUpdate"]["success"].as_bool().unwrap_or(false) {
            anyhow::bail!("issueUpdate returned success=false");
        }
        Ok(())
    }

    /// Add a comment to an issue.
    pub async fn create_comment(&self, issue_id: &str, body: &str) -> Result<LinearComment> {
        const Q: &str = r#"
            mutation($issueId: String!, $body: String!) {
                commentCreate(input: { issueId: $issueId, body: $body }) {
                    success
                    comment { id }
                }
            }
        "#;
        let data = self
            .graphql(Q, json!({ "issueId": issue_id, "body": body }))
            .await?;
        serde_json::from_value(data["commentCreate"]["comment"].clone())
            .context("Failed to parse created comment")
    }

    /// Edit the body of an existing comment.
    pub async fn update_comment(&self, comment_id: &str, body: &str) -> Result<()> {
        const Q: &str = r#"
            mutation($id: String!, $body: String!) {
                commentUpdate(id: $id, input: { body: $body }) {
                    success
                }
            }
        "#;
        self.graphql(Q, json!({ "id": comment_id, "body": body })).await?;
        Ok(())
    }
}
