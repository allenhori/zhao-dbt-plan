//! The core cascading-window algorithm: anchor identification (§4 of the
//! spec), per-model window expansion with edge-specific overrides, and
//! multi-upstream bounding-box union (§6).

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
    /// Its own declared default `lookback_days` (0 if it has no
    /// `config.meta.zhao` block at all -- see §6's "no global default"
    /// decision).
    pub lookback_days: i64,
    /// Its own declared default `lookahead_days`, same zero-default rule.
    pub lookahead_days: i64,
    /// Direct upstream dependencies *within the plan* (not the whole
    /// project graph), by bare name -- see §8.
    pub depends_on: Vec<String>,
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
    /// The Anchor window applied to every Entry Node.
    pub anchor_window: Window,
    /// Where that window came from.
    pub anchor_source: AnchorSource,
    /// Every planned model, in topological order.
    pub models: Vec<PlannedModel>,
    /// Any `max-window-expansion-days` breaches, one per affected model.
    pub warnings: Vec<Warning>,
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
}

/// Builds a [`Plan`] for `selected` (a set of model `unique_id`s, e.g.
/// from [`crate::select::resolve`]) against `manifest`.
///
/// `explicit_window` is `Some((start, end))` when `--event-time-start`/
/// `--event-time-end` were both passed; `None` defaults every Entry
/// Node's window to yesterday (§4).
pub fn build(
    manifest: &Manifest,
    selected: &HashSet<String>,
    explicit_window: Option<(Date, Date)>,
    max_window_expansion_days: i64,
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

    let mut windows: HashMap<&str, Window> = HashMap::new();
    let mut models = Vec::with_capacity(order.len());
    let mut warnings = Vec::new();

    for id in &order {
        let node = &manifest.nodes[*id];
        let deps = &within_selection[id];

        let window = if deps.is_empty() {
            anchor_window
        } else {
            let zhao_meta = node.zhao_meta.clone().unwrap_or_default();
            deps.iter()
                .map(|upstream_id| {
                    let upstream_name = &manifest.nodes[*upstream_id].name;
                    let lookback = zhao_meta
                        .lookback_overrides
                        .get(upstream_name)
                        .copied()
                        .unwrap_or(zhao_meta.lookback_days);
                    let lookahead = zhao_meta
                        .lookahead_overrides
                        .get(upstream_name)
                        .copied()
                        .unwrap_or(zhao_meta.lookahead_days);
                    let upstream_window = windows[upstream_id];
                    Window {
                        start: upstream_window.start.minus_days(lookback),
                        end: upstream_window.end.plus_days(lookahead),
                    }
                })
                .reduce(Window::union)
                .expect("deps is non-empty, checked above")
        };

        windows.insert(id, window);

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
                message: "declares config.meta.zhao (lookback_days/lookahead_days) but has no \
                          event_time configured -- this isn't a microbatch model in dbt's own \
                          terms, so this config has no real effect"
                    .to_string(),
            });
        }

        let zhao_meta = node.zhao_meta.clone().unwrap_or_default();
        models.push(PlannedModel {
            name: node.name.clone(),
            window,
            lookback_days: zhao_meta.lookback_days,
            lookahead_days: zhao_meta.lookahead_days,
            depends_on: deps
                .iter()
                .map(|dep_id| manifest.nodes[*dep_id].name.clone())
                .collect(),
        });
    }

    Ok(Plan {
        anchor_window,
        anchor_source,
        models,
        warnings,
    })
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
            lookback_days: lookback,
            lookahead_days: lookahead,
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
        let plan = build(&manifest, &selected, Some((anchor, anchor)), 90).expect("should build");

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
        let plan = build(&manifest, &selected, Some((anchor, anchor)), 90).expect("should build");

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
        let plan = build(&manifest, &selected, Some((anchor, anchor)), 90).expect("should build");

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
        let plan = build(&manifest, &selected, Some((anchor, anchor)), 90).expect("should build");

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
        let plan = build(&manifest, &selected, Some((anchor, anchor)), 90).expect("should build");

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
        let plan = build(&manifest, &selected, Some((anchor, anchor)), 90).expect("should build");

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
        let plan = build(&manifest, &selected, None, 90).expect("should build");

        assert_eq!(plan.anchor_window.start, Date::yesterday());
        assert!(matches!(plan.anchor_source, AnchorSource::DefaultYesterday));
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
        let plan = build(&manifest, &selected, Some((anchor, anchor)), 90).expect("should build");

        assert_eq!(plan.models.len(), 1);
        assert!(plan.models[0].depends_on.is_empty());
        assert_eq!(
            plan.models[0].window.start.to_string(),
            "2026-07-01",
            "b should be treated as an Entry Node since its only dependency isn't selected"
        );
    }
}
