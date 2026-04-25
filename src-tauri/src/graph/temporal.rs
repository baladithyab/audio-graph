//! Temporal knowledge graph implementation using petgraph.
//!
//! The graph uses `StableGraph` for stable node/edge indices across mutations.
//! Each edge carries temporal metadata (valid_from, valid_until) for
//! time-aware relationship tracking.

use petgraph::stable_graph::{NodeIndex, StableGraph};
use petgraph::visit::EdgeRef;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;

use super::entities::{
    entity_type_color, relation_type_color, ExtractedEntity, ExtractedRelation, ExtractionResult,
    GraphDelta, GraphEdge, GraphEntity, GraphLink, GraphNode, GraphSnapshot, GraphStats,
};

/// Edge data in the temporal graph.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TemporalEdge {
    pub relation_type: String,
    pub valid_from: f64,
    pub valid_until: Option<f64>,
    pub confidence: f32,
    pub source_segment_id: String,
    pub detail: Option<String>,
    /// Strength — incremented on repeated mentions of the same relation.
    pub weight: f32,
}

/// Maximum number of nodes before eviction of oldest (lowest `last_seen`).
const MAX_NODES: usize = 1000;

/// Maximum number of edges before eviction of oldest (lowest `valid_from`).
const MAX_EDGES: usize = 5000;

/// A temporal knowledge graph backed by petgraph's StableGraph.
pub struct TemporalKnowledgeGraph {
    /// The underlying petgraph graph.
    graph: StableGraph<GraphEntity, TemporalEdge>,
    /// Index from entity name (lowercased) to node index.
    name_index: HashMap<String, NodeIndex>,
    /// Event counter for generating unique IDs.
    event_counter: u64,

    // -- Delta tracking state --------------------------------------------------
    /// IDs of nodes added since the last `take_delta()` call.
    delta_added_node_ids: Vec<String>,
    /// IDs of nodes updated (but not newly added) since the last `take_delta()`.
    delta_updated_node_ids: Vec<String>,
    /// (source_idx, target_idx, edge_idx) of edges added since last delta.
    delta_added_edge_indices: Vec<petgraph::graph::EdgeIndex>,
    /// IDs of removed (evicted) nodes since last delta.
    delta_removed_node_ids: Vec<String>,
    /// Synthetic IDs for removed (evicted) edges since last delta.
    delta_removed_edge_ids: Vec<String>,
}

/// Serializable representation of the graph for save/load.
#[derive(Serialize, Deserialize)]
struct SerializableGraph {
    nodes: Vec<GraphEntity>,
    edges: Vec<SerializableEdge>,
    event_counter: u64,
}

/// Serializable edge with source/target names.
#[derive(Serialize, Deserialize)]
struct SerializableEdge {
    source_name: String,
    target_name: String,
    edge: TemporalEdge,
}

impl TemporalKnowledgeGraph {
    /// Create a new empty temporal knowledge graph.
    pub fn new() -> Self {
        Self {
            graph: StableGraph::new(),
            name_index: HashMap::new(),
            event_counter: 0,
            delta_added_node_ids: Vec::new(),
            delta_updated_node_ids: Vec::new(),
            delta_added_edge_indices: Vec::new(),
            delta_removed_node_ids: Vec::new(),
            delta_removed_edge_ids: Vec::new(),
        }
    }

    /// Get the current number of nodes in the graph.
    pub fn node_count(&self) -> usize {
        self.graph.node_count()
    }

    /// Get the current number of edges in the graph.
    pub fn edge_count(&self) -> usize {
        self.graph.edge_count()
    }

    /// Get the current episode count.
    pub fn episode_count(&self) -> u64 {
        self.event_counter
    }

    /// Add or update an entity. If entity name exists (case-insensitive),
    /// update `last_seen` and increment `mention_count`. Returns the `NodeIndex`.
    pub fn add_entity(
        &mut self,
        entity: &ExtractedEntity,
        timestamp: f64,
        speaker: &str,
    ) -> NodeIndex {
        let key = entity.name.to_lowercase();

        if let Some(&idx) = self.name_index.get(&key) {
            // Update existing entity
            if let Some(node) = self.graph.node_weight_mut(idx) {
                node.last_seen = timestamp;
                node.mention_count += 1;
                if !node.speakers.contains(&speaker.to_string()) {
                    node.speakers.push(speaker.to_string());
                }
                // Update description if we have a better one
                if entity.description.is_some() && node.description.is_none() {
                    node.description = entity.description.clone();
                }
                // Track as updated (if not already tracked as newly added)
                if !self.delta_added_node_ids.contains(&node.id) {
                    let id = node.id.clone();
                    if !self.delta_updated_node_ids.contains(&id) {
                        self.delta_updated_node_ids.push(id);
                    }
                }
            }
            idx
        } else {
            // Create new entity
            let id = uuid::Uuid::new_v4().to_string();
            self.delta_added_node_ids.push(id.clone());
            let node = GraphEntity {
                id,
                name: entity.name.clone(),
                entity_type: entity.entity_type.clone(),
                mention_count: 1,
                first_seen: timestamp,
                last_seen: timestamp,
                aliases: vec![],
                description: entity.description.clone(),
                speakers: vec![speaker.to_string()],
            };
            let idx = self.graph.add_node(node);
            self.name_index.insert(key, idx);
            idx
        }
    }

    /// Add a relation between two entities. If the same relation type already
    /// exists between them, increment weight instead of creating a duplicate.
    pub fn add_relation(
        &mut self,
        source_name: &str,
        target_name: &str,
        relation: &ExtractedRelation,
        timestamp: f64,
        segment_id: &str,
    ) {
        let source_key = source_name.to_lowercase();
        let target_key = target_name.to_lowercase();

        let source_idx = match self.name_index.get(&source_key) {
            Some(&idx) => idx,
            None => {
                log::warn!("Source entity '{}' not found in graph", source_name);
                return;
            }
        };
        let target_idx = match self.name_index.get(&target_key) {
            Some(&idx) => idx,
            None => {
                log::warn!("Target entity '{}' not found in graph", target_name);
                return;
            }
        };

        // Check if same relation already exists between these nodes
        let existing_edge = self
            .graph
            .edges_connecting(source_idx, target_idx)
            .find(|e| e.weight().relation_type == relation.relation_type);

        if let Some(edge_ref) = existing_edge {
            let edge_idx = edge_ref.id();
            if let Some(edge) = self.graph.edge_weight_mut(edge_idx) {
                edge.weight += 1.0;
                edge.valid_from = edge.valid_from.min(timestamp); // earliest mention
            }
        } else {
            self.event_counter += 1;
            let edge = TemporalEdge {
                relation_type: relation.relation_type.clone(),
                valid_from: timestamp,
                valid_until: None,
                confidence: 1.0,
                source_segment_id: segment_id.to_string(),
                detail: relation.detail.clone(),
                weight: 1.0,
            };
            let edge_idx = self.graph.add_edge(source_idx, target_idx, edge);
            self.delta_added_edge_indices.push(edge_idx);
        }
    }

    /// Resolve an entity name using fuzzy matching (strsim).
    /// Returns `NodeIndex` if a close match is found above `threshold`.
    pub fn resolve_entity(&self, name: &str, threshold: f64) -> Option<NodeIndex> {
        let key = name.to_lowercase();

        // Exact match first
        if let Some(&idx) = self.name_index.get(&key) {
            return Some(idx);
        }

        // Fuzzy match
        let mut best_match: Option<(NodeIndex, f64)> = None;
        for (existing_name, &idx) in &self.name_index {
            let similarity = strsim::jaro_winkler(&key, existing_name);
            if similarity >= threshold
                && (best_match.is_none() || similarity > best_match.unwrap().1)
            {
                best_match = Some((idx, similarity));
            }
        }

        best_match.map(|(idx, _)| idx)
    }

    /// Invalidate an edge by setting its `valid_until` timestamp (Graphiti
    /// temporal concept).
    pub fn invalidate_edge(&mut self, edge_idx: petgraph::graph::EdgeIndex, timestamp: f64) {
        if let Some(edge) = self.graph.edge_weight_mut(edge_idx) {
            edge.valid_until = Some(timestamp);
        }
    }

    /// Process a full extraction result from a transcript segment.
    /// This is the main entry point for feeding data into the graph.
    ///
    /// After inserting entities and relations, enforces size limits by
    /// evicting the oldest nodes/edges when `MAX_NODES` or `MAX_EDGES`
    /// (see the consts in this module) are exceeded.
    pub fn process_extraction(
        &mut self,
        result: &ExtractionResult,
        timestamp: f64,
        speaker: &str,
        segment_id: &str,
    ) {
        // First, add/update all entities
        for entity in &result.entities {
            self.add_entity(entity, timestamp, speaker);
        }

        // Then, add all relations
        for relation in &result.relations {
            self.add_relation(
                &relation.source,
                &relation.target,
                relation,
                timestamp,
                segment_id,
            );
        }

        self.event_counter += 1;

        // Evict oldest nodes if over limit
        self.evict_excess_nodes();

        // Evict oldest edges if over limit
        self.evict_excess_edges();
    }

    /// Remove the oldest nodes (by `last_seen`) until count ≤ [`MAX_NODES`].
    fn evict_excess_nodes(&mut self) {
        while self.graph.node_count() > MAX_NODES {
            // Find the node with the smallest `last_seen` timestamp
            let oldest = self.graph.node_indices().min_by(|&a, &b| {
                let a_ts = self
                    .graph
                    .node_weight(a)
                    .map(|n| n.last_seen)
                    .unwrap_or(f64::MAX);
                let b_ts = self
                    .graph
                    .node_weight(b)
                    .map(|n| n.last_seen)
                    .unwrap_or(f64::MAX);
                a_ts.partial_cmp(&b_ts).unwrap_or(std::cmp::Ordering::Equal)
            });

            if let Some(idx) = oldest {
                // Remove from name_index before removing from graph
                if let Some(entity) = self.graph.node_weight(idx) {
                    let key = entity.name.to_lowercase();
                    self.name_index.remove(&key);
                    // Track removal in delta
                    self.delta_removed_node_ids.push(entity.id.clone());
                    // Remove from added/updated lists if present
                    self.delta_added_node_ids.retain(|id| id != &entity.id);
                    self.delta_updated_node_ids.retain(|id| id != &entity.id);
                    log::debug!(
                        "Graph eviction: removed oldest node '{}' (last_seen={:.1})",
                        entity.name,
                        entity.last_seen,
                    );
                }
                self.graph.remove_node(idx);
            } else {
                break;
            }
        }
    }

    /// Remove the oldest edges (by `valid_from`) until count ≤ [`MAX_EDGES`].
    fn evict_excess_edges(&mut self) {
        while self.graph.edge_count() > MAX_EDGES {
            // Find the edge with the smallest `valid_from` timestamp
            let oldest = self.graph.edge_indices().min_by(|&a, &b| {
                let a_ts = self
                    .graph
                    .edge_weight(a)
                    .map(|e| e.valid_from)
                    .unwrap_or(f64::MAX);
                let b_ts = self
                    .graph
                    .edge_weight(b)
                    .map(|e| e.valid_from)
                    .unwrap_or(f64::MAX);
                a_ts.partial_cmp(&b_ts).unwrap_or(std::cmp::Ordering::Equal)
            });

            if let Some(idx) = oldest {
                // Track removal in delta with a synthetic ID
                let edge_id = format!("edge-evicted-{:?}", idx);
                self.delta_removed_edge_ids.push(edge_id);
                // Remove from added list if present
                self.delta_added_edge_indices.retain(|&ei| ei != idx);
                log::debug!("Graph eviction: removed oldest edge (idx={:?})", idx);
                self.graph.remove_edge(idx);
            } else {
                break;
            }
        }
    }

    /// Take a snapshot of the current graph state for frontend rendering.
    /// Produces a [`GraphSnapshot`] with `nodes`, `links`, and `stats` fields
    /// compatible with react-force-graph.
    pub fn snapshot(&self) -> GraphSnapshot {
        let nodes: Vec<GraphNode> = self
            .graph
            .node_indices()
            .filter_map(|idx| {
                self.graph.node_weight(idx).map(|entity| GraphNode {
                    id: entity.id.clone(),
                    name: entity.name.clone(),
                    entity_type: entity.entity_type.clone(),
                    val: (entity.mention_count as f32).sqrt() * 2.0 + 1.0,
                    color: entity_type_color(&entity.entity_type).to_string(),
                    first_seen: entity.first_seen,
                    last_seen: entity.last_seen,
                    mention_count: entity.mention_count,
                    description: entity.description.clone(),
                })
            })
            .collect();

        let links: Vec<GraphLink> = self
            .graph
            .edge_indices()
            .filter_map(|idx| {
                let (source_idx, target_idx) = self.graph.edge_endpoints(idx)?;
                let edge = self.graph.edge_weight(idx)?;
                let source_node = self.graph.node_weight(source_idx)?;
                let target_node = self.graph.node_weight(target_idx)?;

                // Only include valid (non-expired) edges
                if edge.valid_until.is_some() {
                    return None;
                }

                Some(GraphLink {
                    source: source_node.id.clone(),
                    target: target_node.id.clone(),
                    relation_type: edge.relation_type.clone(),
                    weight: edge.weight,
                    color: relation_type_color(&edge.relation_type).to_string(),
                    label: edge
                        .detail
                        .clone()
                        .or_else(|| Some(edge.relation_type.clone())),
                })
            })
            .collect();

        GraphSnapshot {
            stats: GraphStats {
                total_nodes: nodes.len(),
                total_edges: links.len(),
                total_episodes: self.event_counter,
            },
            nodes,
            links,
        }
    }

    // -----------------------------------------------------------------------
    // Delta tracking
    // -----------------------------------------------------------------------

    /// Returns `true` if there are any accumulated changes since the last
    /// `take_delta()` call.
    pub fn has_delta(&self) -> bool {
        !self.delta_added_node_ids.is_empty()
            || !self.delta_updated_node_ids.is_empty()
            || !self.delta_added_edge_indices.is_empty()
            || !self.delta_removed_node_ids.is_empty()
            || !self.delta_removed_edge_ids.is_empty()
    }

    /// Take the accumulated delta since the last call, resetting the internal
    /// delta buffers. Returns a [`GraphDelta`] with the changes.
    pub fn take_delta(&mut self) -> GraphDelta {
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs_f64();

        // Collect added nodes
        let added_nodes: Vec<GraphNode> = self
            .delta_added_node_ids
            .drain(..)
            .filter_map(|id| {
                self.graph.node_indices().find_map(|idx| {
                    let entity = self.graph.node_weight(idx)?;
                    if entity.id == id {
                        Some(GraphNode {
                            id: entity.id.clone(),
                            name: entity.name.clone(),
                            entity_type: entity.entity_type.clone(),
                            val: (entity.mention_count as f32).sqrt() * 2.0 + 1.0,
                            color: entity_type_color(&entity.entity_type).to_string(),
                            first_seen: entity.first_seen,
                            last_seen: entity.last_seen,
                            mention_count: entity.mention_count,
                            description: entity.description.clone(),
                        })
                    } else {
                        None
                    }
                })
            })
            .collect();

        // Collect updated nodes
        let updated_nodes: Vec<GraphNode> = self
            .delta_updated_node_ids
            .drain(..)
            .filter_map(|id| {
                self.graph.node_indices().find_map(|idx| {
                    let entity = self.graph.node_weight(idx)?;
                    if entity.id == id {
                        Some(GraphNode {
                            id: entity.id.clone(),
                            name: entity.name.clone(),
                            entity_type: entity.entity_type.clone(),
                            val: (entity.mention_count as f32).sqrt() * 2.0 + 1.0,
                            color: entity_type_color(&entity.entity_type).to_string(),
                            first_seen: entity.first_seen,
                            last_seen: entity.last_seen,
                            mention_count: entity.mention_count,
                            description: entity.description.clone(),
                        })
                    } else {
                        None
                    }
                })
            })
            .collect();

        // Collect added edges
        let added_edges: Vec<GraphEdge> = self
            .delta_added_edge_indices
            .drain(..)
            .filter_map(|edge_idx| {
                let (source_idx, target_idx) = self.graph.edge_endpoints(edge_idx)?;
                let edge = self.graph.edge_weight(edge_idx)?;
                let source_node = self.graph.node_weight(source_idx)?;
                let target_node = self.graph.node_weight(target_idx)?;

                // Skip expired edges
                if edge.valid_until.is_some() {
                    return None;
                }

                Some(GraphEdge {
                    id: format!("edge-{:?}", edge_idx),
                    source: source_node.id.clone(),
                    target: target_node.id.clone(),
                    relation_type: edge.relation_type.clone(),
                    weight: edge.weight,
                    color: relation_type_color(&edge.relation_type).to_string(),
                    label: edge
                        .detail
                        .clone()
                        .or_else(|| Some(edge.relation_type.clone())),
                })
            })
            .collect();

        let removed_node_ids: Vec<String> = self.delta_removed_node_ids.drain(..).collect();
        let removed_edge_ids: Vec<String> = self.delta_removed_edge_ids.drain(..).collect();

        GraphDelta {
            added_nodes,
            updated_nodes,
            added_edges,
            removed_node_ids,
            removed_edge_ids,
            timestamp,
        }
    }

    // -----------------------------------------------------------------------
    // Persistence (save / load)
    // -----------------------------------------------------------------------

    /// Serialize the graph to a JSON file.
    pub fn save_to_file(&self, path: &Path) -> Result<(), String> {
        let nodes: Vec<GraphEntity> = self
            .graph
            .node_indices()
            .filter_map(|idx| self.graph.node_weight(idx).cloned())
            .collect();

        let edges: Vec<SerializableEdge> = self
            .graph
            .edge_indices()
            .filter_map(|idx| {
                let (src, tgt) = self.graph.edge_endpoints(idx)?;
                let edge = self.graph.edge_weight(idx)?.clone();
                let source_name = self.graph.node_weight(src)?.name.clone();
                let target_name = self.graph.node_weight(tgt)?.name.clone();
                Some(SerializableEdge {
                    source_name,
                    target_name,
                    edge,
                })
            })
            .collect();

        let data = SerializableGraph {
            nodes,
            edges,
            event_counter: self.event_counter,
        };

        crate::persistence::save_json(&data, path)
    }

    /// Deserialize a graph from a JSON file.
    pub fn load_from_file(path: &Path) -> Result<Self, String> {
        let data: SerializableGraph = crate::persistence::load_json(path)?;

        let mut graph = StableGraph::new();
        let mut name_index = HashMap::new();

        // Re-create nodes
        for entity in &data.nodes {
            let idx = graph.add_node(entity.clone());
            name_index.insert(entity.name.to_lowercase(), idx);
        }

        // Re-create edges
        for se in &data.edges {
            let src_key = se.source_name.to_lowercase();
            let tgt_key = se.target_name.to_lowercase();
            if let (Some(&src_idx), Some(&tgt_idx)) =
                (name_index.get(&src_key), name_index.get(&tgt_key))
            {
                graph.add_edge(src_idx, tgt_idx, se.edge.clone());
            } else {
                log::warn!(
                    "Graph load: skipping edge '{}' → '{}' (missing node)",
                    se.source_name,
                    se.target_name
                );
            }
        }

        Ok(Self {
            graph,
            name_index,
            event_counter: data.event_counter,
            delta_added_node_ids: Vec::new(),
            delta_updated_node_ids: Vec::new(),
            delta_added_edge_indices: Vec::new(),
            delta_removed_node_ids: Vec::new(),
            delta_removed_edge_ids: Vec::new(),
        })
    }
}

impl Default for TemporalKnowledgeGraph {
    fn default() -> Self {
        Self::new()
    }
}
