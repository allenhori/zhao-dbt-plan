//! The core cascading-window algorithm: anchor identification (§4 of the
//! spec), per-model window expansion with edge-specific overrides, and
//! multi-upstream bounding-box union (§6).
//!
//! ## `--anchor <model>`: pinning the literal window to a named model
//!
//! Without `--anchor`, the literal `--event-time-start`/`--event-time-end`
//! window (or the default-yesterday fallback) applies to every Entry Node
//! (a selected model with no upstream dependency *within the selection*)
//! and cascades forward from there -- this is the original, and remains
//! the default, behavior.
//!
//! With `--anchor <model>`, the literal window instead applies to that
//! one named model, wherever it sits in the selected subgraph:
//!
//! - **Downstream of the anchor**: completely unchanged -- the same
//!   forward-cascade formula below, just starting from the anchor's
//!   window instead of an Entry Node's (the anchor's own window is seeded
//!   into `windows` before the forward pass runs, so the existing loop
//!   needs no awareness of `--anchor` at all for this direction).
//! - **Upstream of the anchor**: the new part -- walked backward from the
//!   anchor, one edge at a time, applying the *same* formula in reverse:
//!   at each hop, the upstream node's needed window is the **downstream**
//!   (closer-to-anchor) node's own window, padded outward by that
//!   downstream node's own `(lookback, lookahead)` config -- same
//!   direction of padding (subtract lookback from start, add lookahead to
//!   end), just walked toward upstream instead of away from it. Computed
//!   in a dedicated pass (`backward_cascade` below) *before* the main
//!   forward pass, into a `windows` seed the forward pass then just reads
//!   like any other already-resolved upstream.
//! - **No path to/from the anchor**: completely untouched -- the normal
//!   Entry-Node/forward-cascade rule applies exactly as if `--anchor`
//!   weren't passed, since such a node is never visited by
//!   `backward_cascade` and never depends (even transitively) on the
//!   anchor.
//!
//! `--anchor` is a single bare model name, not inferred from `--select`'s
//! `+`/graph-operator shape -- deliberately, so this addon never needs to
//! parse any part of dbt's own selector grammar, the same principle
//! `select.rs`'s module doc comment already establishes for `--select`
//! itself (`+model+` is meaningless here without dbt's own grammar rules
//! for combining it with intersections/unions/methods).

use std::collections::{HashMap, HashSet, VecDeque};

use crate::date::Date;
use crate::manifest::Manifest;

/// A resolved `[event_time_start, event_time_end]` window, both ends
/// inclusive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Window {
    /// The window's start date, inclusive.
    pub start: Date,
    /// The window's end date, inclusive.
    pub end: Date,
}

impl Window {
    fn span_days(self) -> i64 {
        self.start.span_days_to(self.end)
    }

    /// The union of `self` and `other` -- `[min(starts), max(ends)]`. See
    /// §6's "Multi-Upstream Window Fusion (Bounding Box Union)."
    fn union(self, other: Window) -> Window {
        Window {
            start: self.start.min(other.start),
            end: self.end.max(other.end),
        }
    }
}

/// Where the Anchor window came from -- recorded in the plan's metadata
/// block (§8) for traceability ("why did the plan come out this way").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnchorSource {
    /// `--event-time-start`/`--event-time-end` were both explicitly
    /// passed.
    Explicit,
    /// Neither was passed; defaulted to yesterday (§4's "Default Daily
    /// Execution").
    DefaultYesterday,
}

/// One model's resolved plan entry.
#[derive(Debug, Clone)]
pub struct PlannedModel {
    /// The model's bare name.
    pub name: String,
    /// Its resolved window.
    pub window: Window,
    /// Its own declared default `lookback` amount (0 if it has no
    /// `config.meta.zhao` block at all -- see §6's "no global default"
    /// decision), and the unit it's measured in.
    pub lookback: i64,
    /// The unit `lookback` is measured in.
    pub lookback_unit: crate::date::TimeUnit,
    /// Its own declared default `lookahead` amount, same zero-default
    /// rule, and the unit it's measured in.
    pub lookahead: i64,
    /// The unit `lookahead` is measured in.
    pub lookahead_unit: crate::date::TimeUnit,
    /// Direct upstream dependencies *within the plan* (not the whole
    /// project graph), by bare name -- see §8.
    pub depends_on: Vec<String>,
    /// Longest-path depth from an Entry Node, within the selected
    /// subgraph only -- an Entry Node (no `depends_on` within the
    /// selection) is layer 0; every other model is `1 + max(every
    /// upstream's layer)`. A diamond dependency (two upstream paths of
    /// different length) still collapses to one number, the longer
    /// path's `+1` -- same "good enough to read the DAG's tiers" call
    /// `render_tree`'s original depth math already made before this
    /// field existed to name it.
    pub layer: usize,
}

/// A non-fatal issue surfaced alongside an otherwise-complete plan (§6's
/// `max-window-expansion-days` ceiling: warn, never fail).
#[derive(Debug, Clone)]
pub struct Warning {
    /// The model the warning is about.
    pub model: String,
    /// A human-readable explanation.
    pub message: String,
}

/// A complete resolved plan.
#[derive(Debug)]
pub struct Plan {
    /// The Anchor window applied to every Entry Node (or, with
    /// `--anchor`, to the named anchor model instead -- see
    /// [`anchor_model`](Self::anchor_model)).
    pub anchor_window: Window,
    /// Where that window came from.
    pub anchor_source: AnchorSource,
    /// The bare name of the model `--anchor` named, if it was given.
    /// `None` for a plan built without `--anchor`, in which case
    /// `anchor_window` applies to every Entry Node as usual.
    pub anchor_model: Option<String>,
    /// Every planned model, in topological order.
    pub models: Vec<PlannedModel>,
    /// Any `max-window-expansion-days` breaches, one per affected model.
    pub warnings: Vec<Warning>,
}

impl Plan {
    /// The human-readable note surfaced whenever `anchor_source` is
    /// [`AnchorSource::DefaultYesterday`] -- shared verbatim across all
    /// three places that must show it unconditionally (stderr in
    /// `main.rs`, the JSON metadata's `anchor_window.note` in
    /// `output.rs`, and the `--html` report's header banner in
    /// `html.rs`), so all three describe the same silent assumption
    /// identically. `None` for `AnchorSource::Explicit` -- nothing to
    /// surface (and, per `--anchor`'s own mandatory-dates rule, this case
    /// never coincides with `anchor_model.is_some()`).
    pub fn default_yesterday_note(&self) -> Option<String> {
        match self.anchor_source {
            AnchorSource::DefaultYesterday => Some(format!(
                "note: --event-time-start/--event-time-end not supplied, defaulting every \
                 Entry Node to yesterday ({})",
                self.anchor_window.start
            )),
            AnchorSource::Explicit => None,
        }
    }
}

/// Everything that can go wrong building a plan from an otherwise-valid
/// selection.
#[derive(Debug, thiserror::Error)]
pub enum PlanError {
    /// The selected subgraph contains a cycle -- shouldn't happen against
    /// a real dbt-compiled manifest (dbt itself refuses to compile a
    /// cycle), but checked defensively rather than looping forever.
    #[error(
        "the selected models form a cycle and have no Entry Node -- this shouldn't be possible \
         against a real dbt-compiled manifest; check that --manifest points at a genuine \
         `dbt compile`/`dbt parse` output"
    )]
    Cycle,
    /// `--anchor <model>` named a model that isn't among `--select`'s
    /// resolved selection (post `dbt ls`).
    #[error(
        "--anchor {anchor:?} is not among the {count} model(s) selected by --select: {selected}"
    )]
    UnknownAnchor {
        /// The `--anchor` value that couldn't be found.
        anchor: String,
        /// How many models were actually selected.
        count: usize,
        /// Their bare names, sorted and comma-separated, for the error
        /// message's "here's what was actually selected" half.
        selected: String,
    },
}

/// Builds a [`Plan`] for `selected` (a set of model `unique_id`s, e.g.
/// from [`crate::select::resolve`]) against `manifest`.
///
/// `explicit_window` is `Some((start, end))` when `--event-time-start`/
/// `--event-time-end` were both passed; `None` defaults every Entry
/// Node's window to yesterday (§4).
///
/// `anchor` is `--anchor`'s bare model name, if given -- see the module
/// doc comment's "`--anchor <model>`" section. Must name a model actually
/// present in `selected`, or this returns [`PlanError::UnknownAnchor`].
/// Callers are expected to have already enforced `--anchor`'s
/// mandatory-dates rule (`explicit_window.is_some()` whenever `anchor` is
/// `Some`) before calling this -- see `main.rs`.
pub fn build(
    manifest: &Manifest,
    selected: &HashSet<String>,
    explicit_window: Option<(Date, Date)>,
    max_window_expansion_days: i64,
    anchor: Option<&str>,
) -> Result<Plan, PlanError> {
    let (anchor_window, anchor_source) = match explicit_window {
        Some((start, end)) => (Window { start, end }, AnchorSource::Explicit),
        None => {
            let yesterday = Date::yesterday();
            (
                Window {
                    start: yesterday,
                    end: yesterday,
                },
                AnchorSource::DefaultYesterday,
            )
        }
    };

    // Edges within the selection only (§4: "no upstream dependencies
    // inside the selection" is what makes a node an Entry Node -- a
    // dependency on something outside the selection, including every
    // Source, doesn't count against it).
    let within_selection: HashMap<&str, Vec<&str>> = selected
        .iter()
        .map(|id| {
            let deps = manifest
                .nodes
                .get(id.as_str())
                .map(|n| {
                    n.depends_on
                        .iter()
                        .filter(|d| selected.contains(d.as_str()))
                        .map(String::as_str)
                        .collect()
                })
                .unwrap_or_default();
            (id.as_str(), deps)
        })
        .collect();

    let order = topological_order(&within_selection)?;

    // `--anchor` resolution: the bare name must match exactly one
    // selected model. Kept as a `&str` borrowed from `selected` itself
    // (not an owned `String`) so its lifetime matches `within_selection`'s
    // -- both are then usable together in `backward_cascade` below.
    let anchor_id: Option<&str> = match anchor {
        Some(name) => Some(resolve_anchor(manifest, selected, name)?),
        None => None,
    };

    // Precomputed here (before the main forward pass below) for every
    // node upstream of the anchor -- see the module doc comment's
    // "`--anchor <model>`" section. Empty when `anchor_id` is `None`, in
    // which case the forward pass below behaves exactly as it always has.
    let windows_seeded_from_anchor =
        backward_cascade(manifest, &within_selection, anchor_id, anchor_window)?;

    let mut windows: HashMap<&str, Window> = HashMap::new();
    // Computed alongside `windows` in the same single topological pass --
    // `order` guarantees every upstream `id` is already in `layers` by
    // the time a downstream node looks it up. See `PlannedModel::layer`'s
    // doc comment for the rule itself.
    let mut layers: HashMap<&str, usize> = HashMap::new();
    let mut models = Vec::with_capacity(order.len());
    let mut warnings = Vec::new();

    for id in &order {
        let node = &manifest.nodes[*id];
        let deps = &within_selection[id];
        // Computed once per node and reused below (both for the window
        // expansion math and the PlannedModel's own recorded
        // lookback/lookahead), rather than cloning it twice.
        let zhao_meta = node.zhao_meta.clone().unwrap_or_default();

        // A node upstream of the anchor (or the anchor itself) already
        // has its window resolved by `backward_cascade` above -- reused
        // here as-is rather than recomputed by the Entry-Node/
        // forward-cascade rule below. Everything else (the anchor's own
        // descendants, and anything with no path to/from the anchor at
        // all) is untouched by `--anchor` and falls through to that rule
        // exactly as if `--anchor` weren't passed.
        let window = if let Some(&seeded) = windows_seeded_from_anchor.get(id) {
            seeded
        } else if deps.is_empty() {
            anchor_window
        } else {
            deps.iter()
                .map(|upstream_id| {
                    let upstream_name = &manifest.nodes[*upstream_id].name;
                    let lookback = zhao_meta
                        .lookback_overrides
                        .get(upstream_name)
                        .copied()
                        .unwrap_or(zhao_meta.lookback);
                    let lookahead = zhao_meta
                        .lookahead_overrides
                        .get(upstream_name)
                        .copied()
                        .unwrap_or(zhao_meta.lookahead);
                    let upstream_window = windows[upstream_id];
                    Window {
                        start: upstream_window
                            .start
                            .minus(lookback, zhao_meta.lookback_unit),
                        end: upstream_window
                            .end
                            .plus(lookahead, zhao_meta.lookahead_unit),
                    }
                })
                .reduce(Window::union)
                .expect("deps is non-empty, checked above")
        };

        windows.insert(id, window);

        let layer = deps
            .iter()
            .map(|dep_id| layers[dep_id])
            .max()
            .map(|d| d + 1)
            .unwrap_or(0);
        layers.insert(id, layer);

        if window.span_days() > max_window_expansion_days {
            warnings.push(Warning {
                model: node.name.clone(),
                message: format!(
                    "expanded window ({} days) exceeds max_window_expansion_days ({})",
                    window.span_days(),
                    max_window_expansion_days
                ),
            });
        }

        // A model that declares lookback/lookahead but no `event_time`
        // at all can't actually be a microbatch model in dbt's own
        // terms -- config.meta.zhao is meaningless without it. Worth a
        // warning, not a hard failure: the plan itself is still
        // computable (and correct, by construction) even if the config
        // it's based on doesn't make sense on dbt's side.
        if node.zhao_meta.is_some() && node.event_time.is_none() {
            warnings.push(Warning {
                model: node.name.clone(),
                message: "declares config.meta.zhao (lookback/lookahead) but has no event_time \
                          configured -- this isn't a microbatch model in dbt's own terms, so \
                          this config has no real effect"
                    .to_string(),
            });
        }

        models.push(PlannedModel {
            name: node.name.clone(),
            window,
            lookback: zhao_meta.lookback,
            lookback_unit: zhao_meta.lookback_unit,
            lookahead: zhao_meta.lookahead,
            lookahead_unit: zhao_meta.lookahead_unit,
            depends_on: deps
                .iter()
                .map(|dep_id| manifest.nodes[*dep_id].name.clone())
                .collect(),
            layer,
        });
    }

    Ok(Plan {
        anchor_window,
        anchor_source,
        anchor_model: anchor.map(str::to_string),
        models,
        warnings,
    })
}

/// Resolves `--anchor`'s bare model name against `selected`, returning
/// the matching `unique_id` (borrowed from `selected` itself, so callers
/// get a `&str` with the same lifetime as `selected`'s own contents).
/// [`PlanError::UnknownAnchor`] otherwise, naming both what was requested
/// and (briefly) what was actually selected -- per the spec's validation
/// requirement.
fn resolve_anchor<'a>(
    manifest: &Manifest,
    selected: &'a HashSet<String>,
    name: &str,
) -> Result<&'a str, PlanError> {
    selected
        .iter()
        .find(|id| {
            manifest
                .nodes
                .get(id.as_str())
                .is_some_and(|n| n.name == name)
        })
        .map(|id| id.as_str())
        .ok_or_else(|| {
            let mut names: Vec<&str> = selected
                .iter()
                .filter_map(|id| manifest.nodes.get(id.as_str()).map(|n| n.name.as_str()))
                .collect();
            names.sort_unstable();
            PlanError::UnknownAnchor {
                anchor: name.to_string(),
                count: names.len(),
                selected: names.join(", "),
            }
        })
}

/// Computes every ancestor-of-anchor's (and the anchor's own) window by
/// walking *backward* from the anchor -- see the module doc comment's
/// "`--anchor <model>`" section for the algorithm. Returns an empty map
/// when `anchor_id` is `None` (no `--anchor` given), in which case the
/// main forward pass in [`build`] is entirely unaffected.
///
/// `within_selection` must be the same upstream-edges-within-the-
/// selection map [`build`] itself uses, so "ancestor of the anchor" means
/// exactly what it means everywhere else in this module.
fn backward_cascade<'a>(
    manifest: &Manifest,
    within_selection: &HashMap<&'a str, Vec<&'a str>>,
    anchor_id: Option<&'a str>,
    anchor_window: Window,
) -> Result<HashMap<&'a str, Window>, PlanError> {
    let Some(anchor_id) = anchor_id else {
        return Ok(HashMap::new());
    };

    // The reverse of `within_selection`: for a given id, who (within the
    // selection) depends on it.
    let mut downstream_within_selection: HashMap<&str, Vec<&str>> = HashMap::new();
    for (id, deps) in within_selection {
        for dep in deps {
            downstream_within_selection
                .entry(*dep)
                .or_default()
                .push(id);
        }
    }

    // Every node the anchor transitively depends on, within the
    // selection -- i.e. everything upstream of it.
    let mut ancestors: HashSet<&str> = HashSet::new();
    let mut frontier: Vec<&str> = within_selection[anchor_id].clone();
    while let Some(id) = frontier.pop() {
        if ancestors.insert(id) {
            frontier.extend(within_selection[id].iter().copied());
        }
    }

    // For the anchor and every one of its ancestors, its "path
    // consumers": the immediate downstream neighbor(s) *on the path back
    // to the anchor* (either the anchor itself, or another ancestor) --
    // never a downstream node that isn't itself upstream of the anchor.
    // Reusing `topological_order` against this map (keyed the same way
    // `within_selection` keys its own deps, just pointed the other
    // direction) gives exactly the processing order this needs: the
    // anchor first (empty path-consumer list, so in-degree zero), then
    // each ancestor only once every consumer of it on the path is
    // already resolved.
    let mut path_consumers_of: HashMap<&str, Vec<&str>> = HashMap::new();
    path_consumers_of.insert(anchor_id, Vec::new());
    for &id in &ancestors {
        let consumers = downstream_within_selection
            .get(id)
            .into_iter()
            .flatten()
            .copied()
            .filter(|consumer| *consumer == anchor_id || ancestors.contains(consumer))
            .collect();
        path_consumers_of.insert(id, consumers);
    }
    let backward_order = topological_order(&path_consumers_of)?;

    let mut windows: HashMap<&str, Window> = HashMap::new();
    windows.insert(anchor_id, anchor_window);
    for id in backward_order {
        if id == anchor_id {
            continue;
        }
        let this_name = &manifest.nodes[id].name;
        let window = path_consumers_of[id]
            .iter()
            .map(|&consumer_id| {
                // The *consumer's* own config, applied against this node
                // (its upstream) -- the exact mirror of the forward
                // formula, which applies the *downstream* node's own
                // config against each of its upstreams. Per-upstream
                // overrides use the same precedence: a consumer's
                // override for this node's name wins over the consumer's
                // own default.
                let consumer_meta = manifest.nodes[consumer_id]
                    .zhao_meta
                    .clone()
                    .unwrap_or_default();
                let lookback = consumer_meta
                    .lookback_overrides
                    .get(this_name)
                    .copied()
                    .unwrap_or(consumer_meta.lookback);
                let lookahead = consumer_meta
                    .lookahead_overrides
                    .get(this_name)
                    .copied()
                    .unwrap_or(consumer_meta.lookahead);
                let consumer_window = windows[consumer_id];
                Window {
                    start: consumer_window
                        .start
                        .minus(lookback, consumer_meta.lookback_unit),
                    end: consumer_window
                        .end
                        .plus(lookahead, consumer_meta.lookahead_unit),
                }
            })
            .reduce(Window::union)
            .expect(
                "id is an ancestor of the anchor, so it has at least one path consumer \
                 (the anchor itself, or another ancestor closer to it)",
            );
        windows.insert(id, window);
    }

    Ok(windows)
}

/// Kahn's algorithm over the selected subgraph, restricted to
/// within-selection edges. Returns [`PlanError::Cycle`] if any node
/// never reaches in-degree zero (impossible against a real dbt-compiled
/// manifest, checked defensively).
fn topological_order<'a>(
    within_selection: &HashMap<&'a str, Vec<&'a str>>,
) -> Result<Vec<&'a str>, PlanError> {
    // in_degree counts each node's own within-selection dependency count
    // -- how many upstream nodes still need to resolve before this one
    // can be placed.
    let mut in_degree: HashMap<&str, usize> = within_selection
        .iter()
        .map(|(id, deps)| (*id, deps.len()))
        .collect();

    let mut initially_ready: Vec<&str> = in_degree
        .iter()
        .filter(|(_, degree)| **degree == 0)
        .map(|(id, _)| *id)
        .collect();
    // Deterministic order regardless of HashMap iteration order.
    initially_ready.sort_unstable();
    let mut queue: VecDeque<&str> = initially_ready.into();

    // downstream index: id -> [ids that depend on it]
    let mut downstream: HashMap<&str, Vec<&str>> = HashMap::new();
    for (id, deps) in within_selection {
        for dep in deps {
            downstream.entry(*dep).or_default().push(id);
        }
    }

    let mut order = Vec::with_capacity(within_selection.len());
    while let Some(id) = queue.pop_front() {
        order.push(id);
        let mut newly_ready = Vec::new();
        if let Some(children) = downstream.get(id) {
            for child in children {
                let degree = in_degree.get_mut(child).expect("child is in selection");
                *degree -= 1;
                if *degree == 0 {
                    newly_ready.push(*child);
                }
            }
        }
        newly_ready.sort_unstable();
        for child in newly_ready {
            queue.push_back(child);
        }
    }

    if order.len() != within_selection.len() {
        return Err(PlanError::Cycle);
    }
    Ok(order)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::{Node, ResourceType, ZhaoMeta};

    fn model(id: &str, name: &str, depends_on: &[&str], zhao_meta: Option<ZhaoMeta>) -> Node {
        Node {
            unique_id: id.to_string(),
            name: name.to_string(),
            resource_type: ResourceType::Model,
            depends_on: depends_on.iter().map(|d| d.to_string()).collect(),
            // Realistic microbatch models always declare event_time
            // alongside config.meta.zhao -- see
            // `an_event_time_less_zhao_meta_config_warns` below for the
            // dedicated test of what happens when they don't.
            event_time: zhao_meta.as_ref().map(|_| "event_time_col".to_string()),
            zhao_meta,
        }
    }

    fn meta(lookback: i64, lookahead: i64) -> ZhaoMeta {
        ZhaoMeta {
            lookback,
            lookback_unit: crate::date::TimeUnit::Day,
            lookahead,
            lookahead_unit: crate::date::TimeUnit::Day,
            lookback_overrides: HashMap::new(),
            lookahead_overrides: HashMap::new(),
        }
    }

    fn manifest_of(nodes: Vec<Node>) -> Manifest {
        Manifest {
            nodes: nodes
                .into_iter()
                .map(|n| (n.unique_id.clone(), n))
                .collect(),
            generated_at: None,
        }
    }

    /// A -> B (lookback=3, lookahead=4) -> C (lookback=2, lookahead=1),
    /// anchored at a single explicit day -- the cascading-expansion
    /// scenario from the spec's §6, with corrected numbers (see
    /// `date::tests::lookback_and_lookahead_expand_in_their_own_correct_direction`
    /// for why the original requirement dump's own worked example was
    /// arithmetically inconsistent, and why these are the right values,
    /// independently cross-checked against Python's `datetime`).
    #[test]
    fn matches_the_corrected_cascading_expansion_example() {
        let manifest = manifest_of(vec![
            model("model.p.a", "a", &[], None),
            model("model.p.b", "b", &["model.p.a"], Some(meta(3, 4))),
            model("model.p.c", "c", &["model.p.b"], Some(meta(2, 1))),
        ]);
        let selected = HashSet::from([
            "model.p.a".to_string(),
            "model.p.b".to_string(),
            "model.p.c".to_string(),
        ]);
        let anchor = Date::parse("2026-07-01").unwrap();
        let plan =
            build(&manifest, &selected, Some((anchor, anchor)), 90, None).expect("should build");

        let by_name: HashMap<&str, &PlannedModel> =
            plan.models.iter().map(|m| (m.name.as_str(), m)).collect();

        assert_eq!(by_name["a"].window.start.to_string(), "2026-07-01");
        assert_eq!(by_name["a"].window.end.to_string(), "2026-07-01");

        assert_eq!(by_name["b"].window.start.to_string(), "2026-06-28");
        assert_eq!(by_name["b"].window.end.to_string(), "2026-07-05");

        assert_eq!(by_name["c"].window.start.to_string(), "2026-06-26");
        assert_eq!(by_name["c"].window.end.to_string(), "2026-07-06");

        assert!(plan.warnings.is_empty());
    }

    #[test]
    fn a_model_with_no_zhao_meta_gets_zero_expansion() {
        let manifest = manifest_of(vec![
            model("model.p.a", "a", &[], None),
            model("model.p.b", "b", &["model.p.a"], None),
        ]);
        let selected = HashSet::from(["model.p.a".to_string(), "model.p.b".to_string()]);
        let anchor = Date::parse("2026-07-01").unwrap();
        let plan =
            build(&manifest, &selected, Some((anchor, anchor)), 90, None).expect("should build");

        let b = plan.models.iter().find(|m| m.name == "b").unwrap();
        assert_eq!(b.window.start.to_string(), "2026-07-01");
        assert_eq!(b.window.end.to_string(), "2026-07-01");
    }

    #[test]
    fn multi_upstream_bounding_box_union_takes_the_min_start_and_max_end() {
        // a and b are both Entry Nodes -- their own zhao_meta (if any)
        // never applies to themselves (only a downstream node's own
        // config, applied against *its* upstreams, ever expands a
        // window). So the asymmetry here has to come from n's own
        // per-upstream overrides instead: 5 days back via a, 5 days
        // forward via b, 0 otherwise.
        let mut n_meta = meta(0, 0);
        n_meta.lookback_overrides.insert("a".to_string(), 5);
        n_meta.lookahead_overrides.insert("b".to_string(), 5);
        let manifest = manifest_of(vec![
            model("model.p.a", "a", &[], None),
            model("model.p.b", "b", &[], None),
            model("model.p.n", "n", &["model.p.a", "model.p.b"], Some(n_meta)),
        ]);
        let selected = HashSet::from([
            "model.p.a".to_string(),
            "model.p.b".to_string(),
            "model.p.n".to_string(),
        ]);
        let anchor = Date::parse("2026-07-01").unwrap();
        let plan =
            build(&manifest, &selected, Some((anchor, anchor)), 90, None).expect("should build");

        let n = plan.models.iter().find(|m| m.name == "n").unwrap();
        // Via a: [06-26, 07-01] (lookback override 5, default lookahead 0).
        // Via b: [07-01, 07-06] (default lookback 0, lookahead override 5).
        // Union: [06-26, 07-06].
        assert_eq!(n.window.start.to_string(), "2026-06-26");
        assert_eq!(n.window.end.to_string(), "2026-07-06");
    }

    #[test]
    fn an_edge_specific_override_wins_over_the_default_for_that_upstream_only() {
        let mut zhao_meta = meta(1, 1);
        zhao_meta.lookback_overrides.insert("a".to_string(), 7);
        let manifest = manifest_of(vec![
            model("model.p.a", "a", &[], None),
            model("model.p.b", "b", &[], None),
            model(
                "model.p.n",
                "n",
                &["model.p.a", "model.p.b"],
                Some(zhao_meta),
            ),
        ]);
        let selected = HashSet::from([
            "model.p.a".to_string(),
            "model.p.b".to_string(),
            "model.p.n".to_string(),
        ]);
        let anchor = Date::parse("2026-07-01").unwrap();
        let plan =
            build(&manifest, &selected, Some((anchor, anchor)), 90, None).expect("should build");

        let n = plan.models.iter().find(|m| m.name == "n").unwrap();
        // Via "a": lookback override 7 -> start 2026-06-24. Via "b":
        // default lookback 1 -> start 2026-06-30. Union takes the
        // earlier one.
        assert_eq!(n.window.start.to_string(), "2026-06-24");
    }

    #[test]
    fn a_window_exceeding_the_ceiling_warns_but_still_produces_a_full_plan() {
        let manifest = manifest_of(vec![
            model("model.p.a", "a", &[], None),
            model("model.p.b", "b", &["model.p.a"], Some(meta(50, 50))),
        ]);
        let selected = HashSet::from(["model.p.a".to_string(), "model.p.b".to_string()]);
        let anchor = Date::parse("2026-07-01").unwrap();
        let plan =
            build(&manifest, &selected, Some((anchor, anchor)), 90, None).expect("should build");

        assert_eq!(plan.models.len(), 2, "the plan is still fully produced");
        assert_eq!(plan.warnings.len(), 1);
        assert_eq!(plan.warnings[0].model, "b");
        assert!(plan.warnings[0].message.contains("101 days"));
        assert!(plan.warnings[0].message.contains("90"));
    }

    #[test]
    fn a_zhao_meta_block_with_no_event_time_warns_but_still_produces_a_plan() {
        let mut b = model("model.p.b", "b", &["model.p.a"], Some(meta(1, 1)));
        b.event_time = None; // declared config.meta.zhao, but not event_time
        let manifest = manifest_of(vec![model("model.p.a", "a", &[], None), b]);
        let selected = HashSet::from(["model.p.a".to_string(), "model.p.b".to_string()]);
        let anchor = Date::parse("2026-07-01").unwrap();
        let plan =
            build(&manifest, &selected, Some((anchor, anchor)), 90, None).expect("should build");

        assert_eq!(plan.models.len(), 2);
        assert_eq!(plan.warnings.len(), 1);
        assert_eq!(plan.warnings[0].model, "b");
        assert!(
            plan.warnings[0].message.contains("event_time"),
            "{}",
            plan.warnings[0].message
        );
    }

    #[test]
    fn no_explicit_window_defaults_every_entry_node_to_yesterday() {
        let manifest = manifest_of(vec![model("model.p.a", "a", &[], None)]);
        let selected = HashSet::from(["model.p.a".to_string()]);
        let plan = build(&manifest, &selected, None, 90, None).expect("should build");

        assert_eq!(plan.anchor_window.start, Date::yesterday());
        assert!(matches!(plan.anchor_source, AnchorSource::DefaultYesterday));
    }

    #[test]
    fn entry_nodes_are_layer_zero_and_layer_increments_along_a_chain() {
        let manifest = manifest_of(vec![
            model("model.p.a", "a", &[], None),
            model("model.p.b", "b", &["model.p.a"], Some(meta(3, 4))),
            model("model.p.c", "c", &["model.p.b"], Some(meta(2, 1))),
        ]);
        let selected = HashSet::from([
            "model.p.a".to_string(),
            "model.p.b".to_string(),
            "model.p.c".to_string(),
        ]);
        let anchor = Date::parse("2026-07-01").unwrap();
        let plan =
            build(&manifest, &selected, Some((anchor, anchor)), 90, None).expect("should build");

        let by_name: HashMap<&str, &PlannedModel> =
            plan.models.iter().map(|m| (m.name.as_str(), m)).collect();
        assert_eq!(by_name["a"].layer, 0);
        assert_eq!(by_name["b"].layer, 1);
        assert_eq!(by_name["c"].layer, 2);
    }

    #[test]
    fn a_diamond_dependency_takes_the_longer_upstream_path_plus_one() {
        // a -> b -> d
        // a -> d (direct, shorter path)
        // d's two upstreams are b (layer 1) and a (layer 0) -- the
        // longer path (through b) must win: d's layer is 2, not 1.
        let manifest = manifest_of(vec![
            model("model.p.a", "a", &[], None),
            model("model.p.b", "b", &["model.p.a"], None),
            model(
                "model.p.d",
                "d",
                &["model.p.a", "model.p.b"],
                Some(meta(1, 1)),
            ),
        ]);
        let selected = HashSet::from([
            "model.p.a".to_string(),
            "model.p.b".to_string(),
            "model.p.d".to_string(),
        ]);
        let anchor = Date::parse("2026-07-01").unwrap();
        let plan =
            build(&manifest, &selected, Some((anchor, anchor)), 90, None).expect("should build");

        let by_name: HashMap<&str, &PlannedModel> =
            plan.models.iter().map(|m| (m.name.as_str(), m)).collect();
        assert_eq!(by_name["a"].layer, 0);
        assert_eq!(by_name["b"].layer, 1);
        assert_eq!(
            by_name["d"].layer, 2,
            "d's layer must come from its longer upstream path (through b), not the shorter \
             direct edge from a"
        );
    }

    #[test]
    fn an_entry_node_created_by_an_unselected_upstream_is_layer_zero() {
        // Same scenario as `depends_on_only_lists_dependencies_within_the_selection`:
        // b's only real dependency (a) isn't selected, so b becomes an
        // Entry Node in this plan and must be layer 0, not treat its
        // unselected upstream as contributing to its depth.
        let manifest = manifest_of(vec![
            model("model.p.a", "a", &[], None),
            model("model.p.b", "b", &["model.p.a"], Some(meta(3, 3))),
        ]);
        let selected = HashSet::from(["model.p.b".to_string()]);
        let anchor = Date::parse("2026-07-01").unwrap();
        let plan =
            build(&manifest, &selected, Some((anchor, anchor)), 90, None).expect("should build");

        assert_eq!(plan.models[0].layer, 0);
    }

    #[test]
    fn depends_on_only_lists_dependencies_within_the_selection() {
        // b depends on a, but a isn't selected -- b becomes an Entry Node
        // in this plan, and its depends_on list must be empty, not
        // reference an unselected model.
        let manifest = manifest_of(vec![
            model("model.p.a", "a", &[], None),
            model("model.p.b", "b", &["model.p.a"], Some(meta(3, 3))),
        ]);
        let selected = HashSet::from(["model.p.b".to_string()]);
        let anchor = Date::parse("2026-07-01").unwrap();
        let plan =
            build(&manifest, &selected, Some((anchor, anchor)), 90, None).expect("should build");

        assert_eq!(plan.models.len(), 1);
        assert!(plan.models[0].depends_on.is_empty());
        assert_eq!(
            plan.models[0].window.start.to_string(),
            "2026-07-01",
            "b should be treated as an Entry Node since its only dependency isn't selected"
        );
    }

    // --- `--anchor <model>` ---------------------------------------------

    /// The exact worked example from the issue: anchor
    /// `mb_orders_rolling_14d` (own config `lookback=2, lookahead=1`),
    /// window fixed at `[2026-01-01, 2026-01-31]`. Its upstream
    /// `mb_orders_rolling_7d`'s needed window is that window padded by
    /// the *anchor's own* config: `[2025-12-30, 2026-02-01]`. One more
    /// hop back, `mb_orders_daily`'s needed window uses
    /// `mb_orders_rolling_7d`'s own `(lookback=3, lookahead=4)`:
    /// `[2025-12-27, 2026-02-05]`.
    #[test]
    fn anchor_backward_cascade_matches_the_worked_example() {
        let manifest = manifest_of(vec![
            model("model.p.daily", "mb_orders_daily", &[], None),
            model(
                "model.p.rolling_7d",
                "mb_orders_rolling_7d",
                &["model.p.daily"],
                Some(meta(3, 4)),
            ),
            model(
                "model.p.rolling_14d",
                "mb_orders_rolling_14d",
                &["model.p.rolling_7d"],
                Some(meta(2, 1)),
            ),
        ]);
        let selected = HashSet::from([
            "model.p.daily".to_string(),
            "model.p.rolling_7d".to_string(),
            "model.p.rolling_14d".to_string(),
        ]);
        let window = (
            Date::parse("2026-01-01").unwrap(),
            Date::parse("2026-01-31").unwrap(),
        );
        let plan = build(
            &manifest,
            &selected,
            Some(window),
            90,
            Some("mb_orders_rolling_14d"),
        )
        .expect("should build");

        let by_name: HashMap<&str, &PlannedModel> =
            plan.models.iter().map(|m| (m.name.as_str(), m)).collect();

        assert_eq!(
            by_name["mb_orders_rolling_14d"].window.start.to_string(),
            "2026-01-01"
        );
        assert_eq!(
            by_name["mb_orders_rolling_14d"].window.end.to_string(),
            "2026-01-31"
        );

        assert_eq!(
            by_name["mb_orders_rolling_7d"].window.start.to_string(),
            "2025-12-30"
        );
        assert_eq!(
            by_name["mb_orders_rolling_7d"].window.end.to_string(),
            "2026-02-01"
        );

        assert_eq!(
            by_name["mb_orders_daily"].window.start.to_string(),
            "2025-12-27"
        );
        assert_eq!(
            by_name["mb_orders_daily"].window.end.to_string(),
            "2026-02-05"
        );

        assert_eq!(plan.anchor_model.as_deref(), Some("mb_orders_rolling_14d"));
    }

    /// Downstream of the anchor: completely unchanged forward-cascade,
    /// just starting from the anchor's literal window instead of an
    /// Entry Node's.
    #[test]
    fn anchor_downstream_cascade_uses_the_existing_forward_formula() {
        let manifest = manifest_of(vec![
            model("model.p.a", "a", &[], None),
            model("model.p.anchor", "anchor", &["model.p.a"], None),
            model("model.p.c", "c", &["model.p.anchor"], Some(meta(3, 4))),
        ]);
        let selected = HashSet::from([
            "model.p.a".to_string(),
            "model.p.anchor".to_string(),
            "model.p.c".to_string(),
        ]);
        let window = (
            Date::parse("2026-07-01").unwrap(),
            Date::parse("2026-07-01").unwrap(),
        );
        let plan =
            build(&manifest, &selected, Some(window), 90, Some("anchor")).expect("should build");

        let by_name: HashMap<&str, &PlannedModel> =
            plan.models.iter().map(|m| (m.name.as_str(), m)).collect();
        assert_eq!(by_name["anchor"].window.start.to_string(), "2026-07-01");
        assert_eq!(by_name["anchor"].window.end.to_string(), "2026-07-01");
        assert_eq!(by_name["c"].window.start.to_string(), "2026-06-28");
        assert_eq!(by_name["c"].window.end.to_string(), "2026-07-05");
    }

    /// Fan-in on the way upstream: a shared upstream feeding two
    /// different nodes both on the path back to the anchor takes the
    /// bounding-box union of what each path independently requires from
    /// it -- the mirrored case of the existing multi-upstream union rule.
    ///
    ///   shared -> left  (lookback=5, lookahead=0) -\
    ///   shared -> right (lookback=0, lookahead=5) --> anchor
    ///
    /// `left`/`right` both depend directly on `shared` and both feed
    /// `anchor` directly, so `shared`'s needed window is the union of
    /// what `left` and `right` each independently require from it.
    #[test]
    fn fan_in_on_the_way_upstream_takes_the_bounding_box_union() {
        let manifest = manifest_of(vec![
            model("model.p.shared", "shared", &[], None),
            model(
                "model.p.left",
                "left",
                &["model.p.shared"],
                Some(meta(5, 0)),
            ),
            model(
                "model.p.right",
                "right",
                &["model.p.shared"],
                Some(meta(0, 5)),
            ),
            model(
                "model.p.anchor",
                "anchor",
                &["model.p.left", "model.p.right"],
                None,
            ),
        ]);
        let selected = HashSet::from([
            "model.p.shared".to_string(),
            "model.p.left".to_string(),
            "model.p.right".to_string(),
            "model.p.anchor".to_string(),
        ]);
        let window = (
            Date::parse("2026-07-01").unwrap(),
            Date::parse("2026-07-01").unwrap(),
        );
        let plan =
            build(&manifest, &selected, Some(window), 90, Some("anchor")).expect("should build");

        let by_name: HashMap<&str, &PlannedModel> =
            plan.models.iter().map(|m| (m.name.as_str(), m)).collect();
        // Via left: [06-26, 07-01] (lookback 5, lookahead 0).
        // Via right: [07-01, 07-06] (lookback 0, lookahead 5).
        // Union: [06-26, 07-06].
        assert_eq!(by_name["shared"].window.start.to_string(), "2026-06-26");
        assert_eq!(by_name["shared"].window.end.to_string(), "2026-07-06");
    }

    /// Per-upstream overrides apply in the backward direction with the
    /// same precedence they already have going forward: a downstream
    /// (here, anchor-side) node's override for a *specific* named
    /// upstream wins over its own default when computing that specific
    /// upstream's needed window.
    #[test]
    fn a_per_upstream_override_applies_correctly_in_the_backward_direction() {
        let mut anchor_meta = meta(1, 1);
        anchor_meta
            .lookback_overrides
            .insert("upstream".to_string(), 9);
        let manifest = manifest_of(vec![
            model("model.p.upstream", "upstream", &[], None),
            model(
                "model.p.anchor",
                "anchor",
                &["model.p.upstream"],
                Some(anchor_meta),
            ),
        ]);
        let selected =
            HashSet::from(["model.p.upstream".to_string(), "model.p.anchor".to_string()]);
        let window = (
            Date::parse("2026-07-01").unwrap(),
            Date::parse("2026-07-01").unwrap(),
        );
        let plan =
            build(&manifest, &selected, Some(window), 90, Some("anchor")).expect("should build");

        let upstream = plan.models.iter().find(|m| m.name == "upstream").unwrap();
        // The override (9) must win over the default lookback (1).
        assert_eq!(upstream.window.start.to_string(), "2026-06-22");
        assert_eq!(upstream.window.end.to_string(), "2026-07-02");
    }

    /// A node in the selection with no path to/from the anchor at all is
    /// completely untouched by `--anchor` -- it keeps today's existing
    /// Entry-Node-based algorithm exactly as if `--anchor` weren't
    /// passed.
    #[test]
    fn a_node_unconnected_to_the_anchor_keeps_the_old_entry_node_behavior() {
        let manifest = manifest_of(vec![
            model("model.p.anchor", "anchor", &[], None),
            model("model.p.unrelated", "unrelated", &[], Some(meta(2, 2))),
        ]);
        let selected = HashSet::from([
            "model.p.anchor".to_string(),
            "model.p.unrelated".to_string(),
        ]);
        let window = (
            Date::parse("2026-07-01").unwrap(),
            Date::parse("2026-07-01").unwrap(),
        );
        let plan =
            build(&manifest, &selected, Some(window), 90, Some("anchor")).expect("should build");

        let unrelated = plan.models.iter().find(|m| m.name == "unrelated").unwrap();
        // unrelated is itself an Entry Node (no deps at all), so it still
        // gets the literal window directly, same as the no-`--anchor` rule.
        assert_eq!(unrelated.window.start.to_string(), "2026-07-01");
        assert_eq!(unrelated.window.end.to_string(), "2026-07-01");
    }

    /// `--anchor` naming a model not present in the resolved selection
    /// produces a clear error naming both the requested anchor and what
    /// was actually selected.
    #[test]
    fn an_anchor_not_in_the_selection_is_a_clear_error() {
        let manifest = manifest_of(vec![
            model("model.p.a", "a", &[], None),
            model("model.p.b", "b", &[], None),
        ]);
        let selected = HashSet::from(["model.p.a".to_string(), "model.p.b".to_string()]);
        let window = (
            Date::parse("2026-07-01").unwrap(),
            Date::parse("2026-07-01").unwrap(),
        );
        let err = build(&manifest, &selected, Some(window), 90, Some("nonexistent")).unwrap_err();

        match &err {
            PlanError::UnknownAnchor {
                anchor,
                count,
                selected,
            } => {
                assert_eq!(anchor, "nonexistent");
                assert_eq!(*count, 2);
                assert!(
                    selected.contains('a') && selected.contains('b'),
                    "{selected}"
                );
            }
            other => panic!("expected UnknownAnchor, got {other:?}"),
        }
        let message = err.to_string();
        assert!(message.contains("nonexistent"), "{message}");
    }

    #[test]
    fn a_plan_built_without_anchor_has_no_anchor_model() {
        let manifest = manifest_of(vec![model("model.p.a", "a", &[], None)]);
        let selected = HashSet::from(["model.p.a".to_string()]);
        let anchor = Date::parse("2026-07-01").unwrap();
        let plan =
            build(&manifest, &selected, Some((anchor, anchor)), 90, None).expect("should build");
        assert_eq!(plan.anchor_model, None);
    }

    #[test]
    fn default_yesterday_note_is_none_for_an_explicit_window() {
        let manifest = manifest_of(vec![model("model.p.a", "a", &[], None)]);
        let selected = HashSet::from(["model.p.a".to_string()]);
        let anchor = Date::parse("2026-07-01").unwrap();
        let plan =
            build(&manifest, &selected, Some((anchor, anchor)), 90, None).expect("should build");
        assert_eq!(plan.default_yesterday_note(), None);
    }

    #[test]
    fn default_yesterday_note_is_present_and_names_the_actual_date_when_defaulted() {
        let manifest = manifest_of(vec![model("model.p.a", "a", &[], None)]);
        let selected = HashSet::from(["model.p.a".to_string()]);
        let plan = build(&manifest, &selected, None, 90, None).expect("should build");
        let note = plan.default_yesterday_note().expect("should have a note");
        assert!(note.contains("yesterday"), "{note}");
        assert!(note.contains(&Date::yesterday().to_string()), "{note}");
    }
}
