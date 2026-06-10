//! `node` entity queries — including the recursive parent_chain CTE that powers
//! US4's `parent_chain` response field and the `chunks/{id}/parents` endpoint.

use mn_core::types::{Node, NodeKind};
use serde::Serialize;
use sqlx::PgPool;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::error::Result;

/// Insert a node, returning the newly-minted id.
///
/// # Errors
///
/// Returns [`crate::error::StoreError::ForeignKeyViolation`] if
/// `source_version_id` or `parent_node_id` are unknown.
pub async fn insert(
    pool: &PgPool,
    source_version_id: Uuid,
    parent_node_id: Option<Uuid>,
    kind: NodeKind,
    name: &str,
    order_index: i32,
) -> Result<Uuid> {
    let kind_str = match kind {
        NodeKind::Root => "root",
        NodeKind::Group => "group",
        NodeKind::Document => "document",
        NodeKind::Chunk => "chunk",
    };
    let row: (Uuid,) = sqlx::query_as(
        "INSERT INTO node (source_version_id, parent_node_id, kind, name, order_index) \
         VALUES ($1, $2, $3, $4, $5) RETURNING id",
    )
    .bind(source_version_id)
    .bind(parent_node_id)
    .bind(kind_str)
    .bind(name)
    .bind(order_index)
    .fetch_one(pool)
    .await?;
    Ok(row.0)
}

/// Walk the parent chain from `node_id` up to the source-version root.
///
/// Returns the chain ordered from immediate parent → root (root last). For a
/// chunk-kind node, the first entry is the document node; subsequent entries
/// are the document's enclosing groups, ending at the implicit root.
///
/// # Errors
///
/// Returns [`crate::error::StoreError::NotFound`] if `node_id` does not exist.
pub async fn parent_chain(pool: &PgPool, node_id: Uuid) -> Result<Vec<Node>> {
    let rows = sqlx::query_as::<_, NodeRow>(
        "WITH RECURSIVE chain AS ( \
             SELECT id, source_version_id, parent_node_id, kind, name, order_index, created_at, 0 AS depth \
             FROM node WHERE id = $1 \
             UNION ALL \
             SELECT n.id, n.source_version_id, n.parent_node_id, n.kind, n.name, n.order_index, n.created_at, c.depth + 1 \
             FROM node n JOIN chain c ON n.id = c.parent_node_id \
         ) \
         SELECT id, source_version_id, parent_node_id, kind, name, order_index, created_at FROM chain \
         WHERE depth > 0 ORDER BY depth",
    )
    .bind(node_id)
    .fetch_all(pool)
    .await?;
    rows.into_iter().map(TryInto::try_into).collect()
}

/// One ancestor in a chunk's parent chain, with the document id attached when
/// the node is a document node (group/root nodes have `document_id: None`).
#[derive(Debug, Clone, Serialize)]
pub struct ParentNode {
    /// Node id (structural hierarchy id — NOT a document id).
    pub id: Uuid,
    /// Owning source version.
    pub source_version_id: Uuid,
    /// Parent node id (`None` for the root).
    pub parent_node_id: Option<Uuid>,
    /// `document` / `group` / `root`.
    pub kind: NodeKind,
    /// Display name (file or folder name; `root` for the root).
    pub name: String,
    /// Sibling order.
    pub order_index: i32,
    /// The fetchable document id, present only on `kind == "document"` nodes.
    pub document_id: Option<Uuid>,
}

/// [`parent_chain`] + a LEFT JOIN to `document` so document-kind nodes carry
/// their fetchable document id. Ordered immediate parent → root.
///
/// # Errors
///
/// Returns [`crate::error::StoreError::Database`] on driver failure, or
/// [`crate::error::StoreError::Json`] if a `node.kind` value fails to decode
/// into [`NodeKind`].
pub async fn parent_chain_with_documents(pool: &PgPool, node_id: Uuid) -> Result<Vec<ParentNode>> {
    #[derive(sqlx::FromRow)]
    struct Row {
        id: Uuid,
        source_version_id: Uuid,
        parent_node_id: Option<Uuid>,
        kind: String,
        name: String,
        order_index: i32,
        document_id: Option<Uuid>,
    }
    let rows = sqlx::query_as::<_, Row>(
        "WITH RECURSIVE chain AS ( \
             SELECT id, source_version_id, parent_node_id, kind, name, order_index, 0 AS depth \
             FROM node WHERE id = $1 \
             UNION ALL \
             SELECT n.id, n.source_version_id, n.parent_node_id, n.kind, n.name, n.order_index, c.depth + 1 \
             FROM node n JOIN chain c ON n.id = c.parent_node_id \
         ) \
         SELECT chain.id, chain.source_version_id, chain.parent_node_id, chain.kind, chain.name, \
                chain.order_index, d.id AS document_id \
         FROM chain LEFT JOIN document d ON d.node_id = chain.id \
         WHERE chain.depth > 0 ORDER BY chain.depth",
    )
    .bind(node_id)
    .fetch_all(pool)
    .await?;
    rows.into_iter()
        .map(|r| {
            let kind: NodeKind = serde_json::from_value(serde_json::Value::String(r.kind))
                .map_err(|e| crate::error::StoreError::Json(e.to_string()))?;
            Ok(ParentNode {
                id: r.id,
                source_version_id: r.source_version_id,
                parent_node_id: r.parent_node_id,
                kind,
                name: r.name,
                order_index: r.order_index,
                document_id: r.document_id,
            })
        })
        .collect()
}

/// Fetch one node by id.
///
/// # Errors
///
/// Returns [`crate::error::StoreError::NotFound`] if id is unknown.
pub async fn get_by_id(pool: &PgPool, id: Uuid) -> Result<Node> {
    let row = sqlx::query_as::<_, NodeRow>(
        "SELECT id, source_version_id, parent_node_id, kind, name, order_index, created_at \
         FROM node WHERE id = $1",
    )
    .bind(id)
    .fetch_one(pool)
    .await?;
    row.try_into()
}

#[derive(sqlx::FromRow)]
struct NodeRow {
    id: Uuid,
    source_version_id: Uuid,
    parent_node_id: Option<Uuid>,
    kind: String,
    name: String,
    order_index: i32,
    created_at: OffsetDateTime,
}

impl TryFrom<NodeRow> for Node {
    type Error = crate::error::StoreError;

    fn try_from(r: NodeRow) -> std::result::Result<Self, Self::Error> {
        let kind: NodeKind = serde_json::from_value(serde_json::Value::String(r.kind))
            .map_err(|e| crate::error::StoreError::Json(e.to_string()))?;
        Ok(Self {
            id: r.id,
            source_version_id: r.source_version_id,
            parent_node_id: r.parent_node_id,
            kind,
            name: r.name,
            order_index: r.order_index,
            created_at: r.created_at,
        })
    }
}
