//! EXPLAIN plans in PostgreSQL's JSON format, as a tree of nodes.

use serde_json::Value;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct PlanNode {
    /// Node type with its target, such as `Index Scan using items_pkey on items`.
    pub title: String,
    /// Conditions, join details, and other properties.
    pub details: Vec<String>,
    pub startup_cost: f64,
    pub total_cost: f64,
    pub plan_rows: f64,
    /// Measured values, present with EXPLAIN ANALYZE.
    pub actual_rows: Option<f64>,
    pub actual_total_ms: Option<f64>,
    pub loops: f64,
    /// Time spent in this node alone, excluding its children, for all loops.
    pub exclusive_ms: Option<f64>,
    pub children: Vec<PlanNode>,
}

impl PlanNode {
    /// Actual rows per loop divided by estimated rows, when it is off by 10x or more.
    pub fn misestimate(&self) -> Option<f64> {
        let actual = self.actual_rows?;
        let estimated = self.plan_rows.max(1.0);
        let ratio = actual.max(1.0) / estimated;
        (!(0.1..=10.0).contains(&ratio)).then_some(ratio)
    }

    pub fn walk<'a>(&'a self, visit: &mut impl FnMut(&'a PlanNode)) {
        visit(self);
        for child in &self.children {
            child.walk(visit);
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Plan {
    pub root: PlanNode,
    pub planning_ms: Option<f64>,
    pub execution_ms: Option<f64>,
    pub analyzed: bool,
}

/// Parses `EXPLAIN (FORMAT JSON)` output.
pub fn parse(json: &str) -> Option<Plan> {
    let value: Value = serde_json::from_str(json).ok()?;
    let top = value.as_array()?.first()?;
    let root = node(top.get("Plan")?);
    let analyzed = root.actual_total_ms.is_some();
    Some(Plan {
        root,
        planning_ms: top.get("Planning Time").and_then(Value::as_f64),
        execution_ms: top.get("Execution Time").and_then(Value::as_f64),
        analyzed,
    })
}

fn node(value: &Value) -> PlanNode {
    let text = |key: &str| value.get(key).and_then(Value::as_str);
    let number = |key: &str| value.get(key).and_then(Value::as_f64);
    let mut title = text("Node Type").unwrap_or("Plan").to_owned();
    // EXPLAIN's text format names ModifyTable nodes by their operation.
    if title == "ModifyTable"
        && let Some(operation) = text("Operation")
    {
        title = operation.to_owned();
    }
    if let Some(strategy) = text("Strategy").filter(|s| *s != "Plain") {
        title = format!("{strategy} {title}");
    }
    if let Some(join) =
        text("Join Type").filter(|_| title.contains("Join") || title.contains("Loop"))
    {
        title = format!("{title} ({join})");
    }
    if let Some(index) = text("Index Name") {
        title.push_str(&format!(" using {index}"));
    }
    if let Some(relation) = text("Relation Name") {
        title.push_str(&format!(" on {relation}"));
        if let Some(alias) = text("Alias").filter(|alias| *alias != relation) {
            title.push_str(&format!(" {alias}"));
        }
    } else if let Some(name) = text("CTE Name").or(text("Function Name")) {
        title.push_str(&format!(" on {name}"));
    }
    let mut details = Vec::new();
    for key in [
        "Index Cond",
        "Recheck Cond",
        "Hash Cond",
        "Merge Cond",
        "Join Filter",
        "Filter",
        "Sort Key",
        "Group Key",
    ] {
        match value.get(key) {
            Some(Value::String(condition)) => details.push(format!("{key}: {condition}")),
            Some(Value::Array(items)) => details.push(format!(
                "{key}: {}",
                items
                    .iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join(", ")
            )),
            _ => {}
        }
    }
    for key in [
        "Rows Removed by Filter",
        "Rows Removed by Index Recheck",
        "Rows Removed by Join Filter",
    ] {
        if let Some(rows) = number(key).filter(|rows| *rows > 0.0) {
            details.push(format!("{key}: {rows}"));
        }
    }
    if let (Some(hit), Some(read)) = (number("Shared Hit Blocks"), number("Shared Read Blocks"))
        && hit + read > 0.0
    {
        details.push(format!("Buffers: {hit} hit, {read} read"));
    }
    if let Some(method) = text("Sort Method") {
        let space = number("Sort Space Used")
            .map(|kb| format!(", {kb} kB"))
            .unwrap_or_default();
        details.push(format!("Sort: {method}{space}"));
    }
    let children: Vec<PlanNode> = value
        .get("Plans")
        .and_then(Value::as_array)
        .map(|plans| plans.iter().map(node).collect())
        .unwrap_or_default();
    let loops = number("Actual Loops").unwrap_or(1.0);
    let actual_total_ms = number("Actual Total Time");
    let exclusive_ms = actual_total_ms.map(|total| {
        let children_ms: f64 = children
            .iter()
            .filter_map(|child| child.actual_total_ms.map(|ms| ms * child.loops))
            .sum();
        (total * loops - children_ms).max(0.0)
    });
    PlanNode {
        title,
        details,
        startup_cost: number("Startup Cost").unwrap_or_default(),
        total_cost: number("Total Cost").unwrap_or_default(),
        plan_rows: number("Plan Rows").unwrap_or_default(),
        actual_rows: number("Actual Rows"),
        actual_total_ms,
        loops,
        exclusive_ms,
        children,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ANALYZED: &str = r#"[{"Plan": {"Node Type": "Hash Join", "Join Type": "Inner", "Startup Cost": 1.0, "Total Cost": 30.5, "Plan Rows": 10, "Plan Width": 8,
        "Actual Startup Time": 0.1, "Actual Total Time": 5.0, "Actual Rows": 500, "Actual Loops": 1, "Hash Cond": "(o.customer_id = c.id)",
        "Plans": [
          {"Node Type": "Seq Scan", "Relation Name": "orders", "Alias": "o", "Startup Cost": 0, "Total Cost": 20, "Plan Rows": 1000, "Plan Width": 8,
           "Actual Startup Time": 0.01, "Actual Total Time": 2.0, "Actual Rows": 1000, "Actual Loops": 1, "Filter": "(total > 0)", "Rows Removed by Filter": 3},
          {"Node Type": "Hash", "Startup Cost": 0, "Total Cost": 1, "Plan Rows": 2, "Plan Width": 8, "Actual Startup Time": 0.5, "Actual Total Time": 0.5, "Actual Rows": 2, "Actual Loops": 1,
           "Plans": [{"Node Type": "Index Scan", "Index Name": "customers_pkey", "Relation Name": "customers", "Alias": "c", "Startup Cost": 0, "Total Cost": 1, "Plan Rows": 2, "Plan Width": 8,
             "Actual Startup Time": 0.2, "Actual Total Time": 0.25, "Actual Rows": 1, "Actual Loops": 2}]}
        ]}, "Planning Time": 0.3, "Execution Time": 5.2}]"#;

    #[test]
    fn parses_nodes_conditions_and_timings() {
        let plan = parse(ANALYZED).unwrap();
        assert!(plan.analyzed);
        assert_eq!(plan.execution_ms, Some(5.2));
        let root = &plan.root;
        assert_eq!(root.title, "Hash Join (Inner)");
        assert_eq!(root.details, ["Hash Cond: (o.customer_id = c.id)"]);
        assert_eq!(root.exclusive_ms, Some(2.5));
        assert_eq!(root.misestimate(), Some(50.0));
        let scan = &root.children[0];
        assert_eq!(scan.title, "Seq Scan on orders o");
        assert!(
            scan.details
                .contains(&"Rows Removed by Filter: 3".to_string())
        );
        let index = &root.children[1].children[0];
        assert_eq!(
            index.title,
            "Index Scan using customers_pkey on customers c"
        );
        assert_eq!(index.exclusive_ms, Some(0.5));
        assert_eq!(root.children[1].exclusive_ms, Some(0.0));
        let mut count = 0;
        root.walk(&mut |_| count += 1);
        assert_eq!(count, 4);
    }

    #[test]
    fn modify_nodes_use_their_operation() {
        let plan = parse(r#"[{"Plan": {"Node Type": "ModifyTable", "Operation": "Delete", "Relation Name": "items", "Alias": "items", "Startup Cost": 0, "Total Cost": 1, "Plan Rows": 0, "Plan Width": 0}}]"#).unwrap();
        assert_eq!(plan.root.title, "Delete on items");
    }

    #[test]
    fn plain_plans_have_no_timings() {
        let plan = parse(r#"[{"Plan": {"Node Type": "Result", "Startup Cost": 0.0, "Total Cost": 0.01, "Plan Rows": 1, "Plan Width": 4}}]"#).unwrap();
        assert!(!plan.analyzed);
        assert_eq!(plan.root.title, "Result");
        assert!(plan.root.misestimate().is_none());
        assert!(parse("not json").is_none());
    }
}
