//! Entity and relation type definitions for the knowledge graph.
//!
//! These types are serialized to JSON and sent to the frontend.

use serde::{Deserialize, Serialize};

/// A node in the knowledge graph representing a named entity.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphEntity {
    /// Stable node ID.
    pub id: String,
    /// Display name.
    pub name: String,
    /// Entity type: Person, Organization, Location, Event, Topic, Product, etc.
    pub entity_type: String,
    /// Number of times this entity has been mentioned.
    pub mention_count: u32,
    /// Timestamp of first mention (seconds since capture start).
    pub first_seen: f64,
    /// Timestamp of most recent mention.
    pub last_seen: f64,
    /// Alternative names / spellings.
    pub aliases: Vec<String>,
    /// Optional description for the entity.
    pub description: Option<String>,
    /// Which speakers mentioned this entity.
    pub speakers: Vec<String>,
}

/// An edge in the knowledge graph representing a relationship between entities.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphRelation {
    /// Stable edge ID.
    pub id: String,
    /// Source entity ID.
    pub source_id: String,
    /// Target entity ID.
    pub target_id: String,
    /// Relationship type: WORKS_AT, LOCATED_IN, KNOWS, etc.
    pub relation_type: String,
    /// When this relationship became valid.
    pub valid_from: f64,
    /// When this relationship ceased to be valid (None = still valid).
    pub valid_until: Option<f64>,
    /// Extraction confidence score.
    pub confidence: f32,
    /// ID of the transcript segment that sourced this relation.
    pub source_segment_id: String,
}

// ---------------------------------------------------------------------------
// Frontend-friendly snapshot types (react-force-graph compatible)
// ---------------------------------------------------------------------------

/// A graph node ready for react-force-graph rendering.
#[derive(Debug, Clone, Serialize)]
pub struct GraphNode {
    pub id: String,
    pub name: String,
    pub entity_type: String,
    /// Node size (based on mention_count).
    pub val: f32,
    /// Color by entity_type.
    pub color: String,
    pub first_seen: f64,
    pub last_seen: f64,
    pub mention_count: u32,
    pub description: Option<String>,
}

/// A graph link ready for react-force-graph rendering.
#[derive(Debug, Clone, Serialize)]
pub struct GraphLink {
    /// Source node id.
    pub source: String,
    /// Target node id.
    pub target: String,
    pub relation_type: String,
    pub weight: f32,
    pub color: String,
    pub label: Option<String>,
}

/// Aggregate graph statistics.
#[derive(Debug, Clone, Serialize, Default)]
pub struct GraphStats {
    pub total_nodes: usize,
    pub total_edges: usize,
    pub total_episodes: u64,
}

/// A point-in-time snapshot of the knowledge graph for frontend rendering.
#[derive(Debug, Clone, Serialize, Default)]
pub struct GraphSnapshot {
    /// All nodes in react-force-graph format.
    pub nodes: Vec<GraphNode>,
    /// All links in react-force-graph format.
    pub links: Vec<GraphLink>,
    /// Aggregate statistics.
    pub stats: GraphStats,
}

/// Delta update for the knowledge graph (incremental changes since last delta).
///
/// Emitted via the `GRAPH_DELTA` event to avoid sending the full snapshot on
/// every extraction cycle. The frontend can apply these deltas to its local
/// graph state for efficient updates.
#[derive(Debug, Clone, Serialize, Default)]
pub struct GraphDelta {
    /// Nodes added since the last delta.
    pub added_nodes: Vec<GraphNode>,
    /// Nodes that were updated (e.g. mention_count changed) since the last delta.
    pub updated_nodes: Vec<GraphNode>,
    /// Edges added since the last delta.
    pub added_edges: Vec<GraphEdge>,
    /// IDs of nodes removed (evicted) since the last delta.
    pub removed_node_ids: Vec<String>,
    /// IDs of edges removed (evicted) since the last delta.
    pub removed_edge_ids: Vec<String>,
    /// Timestamp of this delta.
    pub timestamp: f64,
}

/// A single edge in delta format, carrying source/target node IDs for the
/// frontend to create links.
#[derive(Debug, Clone, Serialize)]
pub struct GraphEdge {
    /// Unique edge identifier.
    pub id: String,
    /// Source node ID.
    pub source: String,
    /// Target node ID.
    pub target: String,
    /// Relationship type.
    pub relation_type: String,
    /// Edge weight (strength).
    pub weight: f32,
    /// Display color.
    pub color: String,
    /// Optional label.
    pub label: Option<String>,
}

/// Result of entity extraction from a transcript segment (from native LLM or rule-based).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtractionResult {
    pub entities: Vec<ExtractedEntity>,
    pub relations: Vec<ExtractedRelation>,
}

/// A raw entity extracted from text (before resolution).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtractedEntity {
    pub name: String,
    /// Entity type: "Person", "Organization", "Location", "Event", "Topic", "Product".
    pub entity_type: String,
    #[serde(default)]
    pub description: Option<String>,
}

/// A raw relation extracted from text (before graph insertion).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtractedRelation {
    pub source: String,
    pub target: String,
    pub relation_type: String,
    #[serde(default)]
    pub detail: Option<String>,
}

// ---------------------------------------------------------------------------
// Color helpers
// ---------------------------------------------------------------------------

/// Map an entity type to a hex color string.
pub fn entity_type_color(entity_type: &str) -> &'static str {
    match entity_type.to_lowercase().as_str() {
        "person" => "#4CAF50",
        "organization" => "#2196F3",
        "location" => "#FF9800",
        "event" => "#9C27B0",
        "topic" => "#00BCD4",
        "product" => "#F44336",
        _ => "#607D8B",
    }
}

/// Map a relation type to a hex color string.
pub fn relation_type_color(relation_type: &str) -> &'static str {
    match relation_type.to_lowercase().as_str() {
        "works_at" | "employed_by" => "#4CAF50",
        "discussed" | "mentioned" => "#2196F3",
        "located_in" | "based_in" => "#FF9800",
        "related_to" => "#9E9E9E",
        _ => "#757575",
    }
}
