//! Server-wide views: session activity, roles, and EXPLAIN plans.

use super::*;
use crate::ui::explain::{Plan, PlanNode};

impl PosternApp {
    pub(super) fn open_activity(&mut self) {
        if self.connected.is_none() || self.blocked_by_edits() {
            return;
        }
        self.mode = Mode::Activity;
        self.send(Command::Activity, "Loading sessions...");
        self.activity_refreshed = Instant::now();
    }

    pub(super) fn open_roles(&mut self) {
        if self.connected.is_none() || self.blocked_by_edits() {
            return;
        }
        self.mode = Mode::Roles;
        self.send(Command::Roles, "Loading roles...");
    }

    pub(super) fn activity_view(&mut self, ui: &mut egui::Ui) {
        let data_busy = self.busy.blocks(Lane::Data);
        if self.activity_auto
            && !data_busy
            && self.activity_refreshed.elapsed() > Duration::from_secs(2)
        {
            self.activity_refreshed = Instant::now();
            self.send(Command::Activity, "Refreshing sessions...");
        }
        if self.activity_auto {
            ui.ctx().request_repaint_after(Duration::from_millis(500));
        }
        let mut signal = None;
        egui::Panel::top("activity_bar")
            .frame(
                egui::Frame::new()
                    .fill(theme::background())
                    .inner_margin(10),
            )
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.heading("Activity");
                    ui.label(
                        RichText::new(format!("{} client sessions", self.activity.len()))
                            .color(theme::muted()),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui
                            .add_enabled(!data_busy, egui::Button::new("Refresh"))
                            .clicked()
                        {
                            self.activity_refreshed = Instant::now();
                            self.send(Command::Activity, "Refreshing sessions...");
                        }
                        ui.checkbox(&mut self.activity_auto, "Refresh every 2 seconds");
                    });
                });
            });
        egui::ScrollArea::both().auto_shrink([false, false]).show(ui, |ui| {
            egui::Frame::new().inner_margin(12).show(ui, |ui| {
                egui::Grid::new("activity")
                    .striped(true)
                    .spacing([16.0, 8.0])
                    .show(ui, |ui| {
                        for title in ["PID", "User", "Database", "Application", "Client", "State", "Wait", "Query time", "Transaction", "Query", ""] {
                            ui.strong(title);
                        }
                        ui.end_row();
                        for session in &self.activity {
                            let long = session.query_seconds.is_some_and(|s| s > 60.0)
                                && session.state == "active";
                            ui.label(RichText::new(session.pid.to_string()).monospace());
                            ui.label(&session.user);
                            ui.label(&session.database);
                            ui.label(if session.own {
                                RichText::new(format!("{} (this app)", session.application)).color(theme::accent())
                            } else {
                                RichText::new(&session.application)
                            });
                            ui.label(&session.client);
                            ui.label(match session.state.as_str() {
                                "active" => RichText::new("active").color(theme::green()),
                                "idle in transaction" | "idle in transaction (aborted)" => {
                                    RichText::new(&session.state).color(theme::gold())
                                }
                                other => RichText::new(other).color(theme::muted()),
                            });
                            ui.label(RichText::new(&session.wait).small().color(theme::muted()));
                            let duration = |seconds: Option<f64>| {
                                seconds.map_or_else(String::new, |seconds| {
                                    let seconds = seconds.max(0.0);
                                    if seconds >= 60.0 {
                                        format!(
                                            "{:.0} min {:.0} s",
                                            (seconds / 60.0).floor(),
                                            seconds % 60.0
                                        )
                                    } else {
                                        format!("{seconds:.1} s")
                                    }
                                })
                            };
                            ui.label(if long {
                                RichText::new(duration(session.query_seconds)).color(theme::gold())
                            } else {
                                RichText::new(duration(session.query_seconds))
                            });
                            ui.label(duration(session.transaction_seconds));
                            let query: String = session
                                .query
                                .split_whitespace()
                                .collect::<Vec<_>>()
                                .join(" ");
                            ui.add_sized(
                                [420.0, 20.0],
                                egui::Label::new(RichText::new(&query).monospace().small())
                                    .truncate(),
                            )
                            .on_hover_text(&session.query);
                            ui.horizontal(|ui| {
                                let enabled = !data_busy && session.state == "active";
                                if ui
                                    .add_enabled(enabled, egui::Button::new("Cancel").small())
                                    .on_hover_text("Cancel the running query (pg_cancel_backend)")
                                    .clicked()
                                {
                                    signal = Some((session.pid, false));
                                }
                                if ui
                                    .add_enabled(!data_busy, egui::Button::new("Terminate...").small())
                                    .on_hover_text("End the session (pg_terminate_backend)")
                                    .clicked()
                                {
                                    self.dialog = Some(Dialog::Terminate {
                                        pid: session.pid,
                                        description: format!(
                                            "{} on {} ({})",
                                            session.user, session.database, session.application
                                        ),
                                    });
                                }
                            });
                            ui.end_row();
                        }
                    });
                if self.activity.is_empty() {
                    ui.label(RichText::new("No sessions visible. Viewing other users' sessions requires pg_read_all_stats.").color(theme::muted()));
                }
            });
        });
        if let Some((pid, terminate)) = signal {
            self.send(
                Command::SignalBackend(pid, terminate),
                "Cancelling query...",
            );
        }
    }

    pub(super) fn roles_view(&mut self, ui: &mut egui::Ui) {
        egui::Panel::top("roles_bar")
            .frame(
                egui::Frame::new()
                    .fill(theme::background())
                    .inner_margin(10),
            )
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.heading("Roles");
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui
                            .add_enabled(
                                !self.busy.blocks(Lane::Data),
                                egui::Button::new("Refresh"),
                            )
                            .clicked()
                        {
                            self.send(Command::Roles, "Loading roles...");
                        }
                    });
                });
            });
        egui::ScrollArea::both()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                egui::Frame::new().inner_margin(12).show(ui, |ui| {
                    egui::Grid::new("roles")
                        .striped(true)
                        .spacing([18.0, 8.0])
                        .show(ui, |ui| {
                            for title in [
                                "Role",
                                "Login",
                                "Superuser",
                                "Create DB",
                                "Create role",
                                "Replication",
                                "Bypass RLS",
                                "Connections",
                                "Valid until",
                                "Member of",
                            ] {
                                ui.strong(title);
                            }
                            ui.end_row();
                            let flag = |ui: &mut egui::Ui, value: bool| {
                                ui.label(if value {
                                    RichText::new("Yes").color(theme::green())
                                } else {
                                    RichText::new("—").color(theme::muted())
                                });
                            };
                            for role in &self.roles {
                                ui.label(RichText::new(&role.name).strong());
                                flag(ui, role.login);
                                flag(ui, role.superuser);
                                flag(ui, role.create_database);
                                flag(ui, role.create_role);
                                flag(ui, role.replication);
                                flag(ui, role.bypass_rls);
                                ui.label(if role.connection_limit < 0 {
                                    "unlimited".to_owned()
                                } else {
                                    role.connection_limit.to_string()
                                });
                                ui.label(role.valid_until.as_deref().unwrap_or(""));
                                ui.label(role.member_of.join(", "));
                                ui.end_row();
                            }
                        });
                });
            });
    }

    /// Runs EXPLAIN for the selection or the statement at the cursor. With
    /// `analyze`, the statement executes inside a transaction or savepoint that is
    /// rolled back, so data changes are not kept.
    ///
    /// A statement with positional parameters such as `$1`, as recorded by
    /// `pg_stat_statements`, gets a generic plan (PostgreSQL 16 and later) and
    /// cannot be analyzed.
    pub(super) fn run_explain(&mut self, analyze: bool) {
        let id = self.documents[self.document_index].id;
        if self.busy.blocks(Lane::Sql(id)) || self.connected.is_none() || self.mode != Mode::Query {
            return;
        }
        let Some((statement, base)) = self.current_statement() else {
            return;
        };
        if lexer::split_statements(&statement).len() != 1 {
            self.warning = Some("Select a single statement to explain.".into());
            return;
        }
        let generic = lexer::has_parameters(&statement);
        if analyze && generic {
            self.warning = Some(
                "Statements with $1 parameters cannot be analyzed. Replace the parameters with values, or use Explain for a generic plan.".into(),
            );
            return;
        }
        let state = self.transaction.get(&id).copied().unwrap_or_default();
        if analyze && state == TransactionState::Failed {
            self.warning =
                Some("The transaction has failed. Run ROLLBACK before EXPLAIN ANALYZE.".into());
            return;
        }
        let options = if analyze {
            "ANALYZE, BUFFERS, FORMAT JSON"
        } else if generic {
            "GENERIC_PLAN, FORMAT JSON"
        } else {
            "FORMAT JSON"
        };
        let explain = format!(
            "EXPLAIN ({options}) {}",
            statement.trim().trim_end_matches(';')
        );
        let (script, wrapped) = match (analyze, state) {
            (false, _) => (explain, false),
            (true, TransactionState::Open) => (
                format!(
                    "SAVEPOINT pgm_explain;\n{explain};\nROLLBACK TO SAVEPOINT pgm_explain;\nRELEASE SAVEPOINT pgm_explain"
                ),
                false,
            ),
            (true, _) => (format!("BEGIN;\n{explain};\nROLLBACK"), true),
        };
        let prefix = lexer::byte_to_char(&script, script.find(statement.trim()).unwrap_or(0));
        if self.send(Command::QueryIn(id, script), "Explaining...") {
            self.running.insert(
                id,
                RunningQuery {
                    base,
                    prefix,
                    kind: QueryKind::Explain { rollback: wrapped },
                },
            );
            self.query_outputs.remove(&id);
            self.plans.remove(&id);
        }
    }

    pub(super) fn plan_view(&mut self, ui: &mut egui::Ui, plan: &Plan) {
        ui.horizontal(|ui| {
            match (plan.planning_ms, plan.execution_ms) {
                (Some(planning), Some(execution)) => {
                    ui.label(format!(
                        "Planning {planning:.3} ms · Execution {execution:.3} ms"
                    ));
                }
                _ => {
                    ui.label(format!("Estimated total cost {:.2}", plan.root.total_cost));
                }
            }
            if !plan.analyzed {
                ui.label(
                    RichText::new("Estimates only. Use Explain Analyze to measure.")
                        .small()
                        .color(theme::muted()),
                );
            }
            if ui.small_button("Copy plan").clicked() {
                let mut text = String::new();
                plan_text(&plan.root, 0, &mut text);
                ui.ctx().copy_text(text);
            }
        });
        ui.separator();
        let total = if plan.analyzed {
            plan.execution_ms
                .or(plan.root.actual_total_ms)
                .unwrap_or(1.0)
                .max(f64::EPSILON)
        } else {
            plan.root.total_cost.max(f64::EPSILON)
        };
        egui::ScrollArea::both()
            .id_salt("plan_scroll")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                plan_node(ui, &plan.root, plan.analyzed, total, "plan")
            });
    }

    /// The selection, or the statement at the cursor, with its character offset.
    pub(super) fn current_statement(&self) -> Option<(String, usize)> {
        let text = &self.documents[self.document_index].sql;
        let selection = lexer::char_to_byte(text, self.editor_selection.start)
            ..lexer::char_to_byte(text, self.editor_selection.end);
        if !selection.is_empty() {
            return Some((
                text[selection.clone()].to_owned(),
                self.editor_selection.start,
            ));
        }
        let cursor = lexer::char_to_byte(text, self.editor_cursor);
        let range = lexer::statement_at(text, cursor)?;
        Some((
            text[range.clone()].to_owned(),
            lexer::byte_to_char(text, range.start),
        ))
    }
}

fn plan_node(ui: &mut egui::Ui, node: &PlanNode, analyzed: bool, total: f64, path: &str) {
    let children_cost: f64 = node.children.iter().map(|c| c.total_cost).sum();
    let share = if analyzed {
        node.exclusive_ms.unwrap_or_default() / total
    } else {
        (node.total_cost - children_cost).max(0.0) / total
    }
    .clamp(0.0, 1.0);
    let header = |ui: &mut egui::Ui| {
        ui.horizontal(|ui| {
            ui.add(
                egui::ProgressBar::new(share as f32)
                    .desired_width(70.0)
                    .text(format!("{:.0}%", share * 100.0)),
            );
            ui.strong(&node.title);
            let mut metrics = format!(
                "cost {:.2}..{:.2} · {} rows",
                node.startup_cost, node.total_cost, node.plan_rows
            );
            if let (Some(ms), Some(rows)) = (node.actual_total_ms, node.actual_rows) {
                metrics.push_str(&format!(
                    " · actual {ms:.3} ms · {rows} rows × {} loops",
                    node.loops
                ));
            }
            ui.label(RichText::new(metrics).small().color(theme::muted()));
            if let Some(ratio) = node.misestimate() {
                let text = if ratio > 1.0 {
                    format!("{ratio:.0}× more rows than estimated")
                } else {
                    format!("{:.0}× fewer rows than estimated", 1.0 / ratio)
                };
                ui.label(RichText::new(text).small().color(theme::gold()))
                    .on_hover_text("Large estimate errors often lead to poor plans. Consider ANALYZE or extended statistics.");
            }
        });
    };
    if node.children.is_empty() && node.details.is_empty() {
        header(ui);
        return;
    }
    let id = ui.make_persistent_id(path);
    egui::collapsing_header::CollapsingState::load_with_default_open(ui.ctx(), id, true)
        .show_header(ui, header)
        .body(|ui| {
            for detail in &node.details {
                ui.label(
                    RichText::new(detail)
                        .small()
                        .monospace()
                        .color(theme::muted()),
                );
            }
            for (index, child) in node.children.iter().enumerate() {
                plan_node(ui, child, analyzed, total, &format!("{path}/{index}"));
            }
        });
}

fn plan_text(node: &PlanNode, depth: usize, out: &mut String) {
    let indent = "  ".repeat(depth);
    out.push_str(&format!(
        "{indent}{} (cost={:.2}..{:.2} rows={})",
        node.title, node.startup_cost, node.total_cost, node.plan_rows
    ));
    if let (Some(ms), Some(rows)) = (node.actual_total_ms, node.actual_rows) {
        out.push_str(&format!(
            " (actual time={ms:.3} rows={rows} loops={})",
            node.loops
        ));
    }
    out.push('\n');
    for detail in &node.details {
        out.push_str(&format!("{indent}  {detail}\n"));
    }
    for child in &node.children {
        plan_text(child, depth + 1, out);
    }
}
