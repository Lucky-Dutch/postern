use super::*;

impl PosternApp {
    pub(super) fn toolbar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal_centered(|ui| {
            ui.add_enabled_ui(!self.busy.control && self.connected.is_some(), |ui| {
                if ui
                    .button(if self.servers_visible {
                        "Workspace"
                    } else {
                        "Servers"
                    })
                    .clicked()
                    && self.connected.is_some()
                {
                    self.servers_visible = !self.servers_visible;
                }
            });
            ui.add_space(10.0);
            ui.vertical(|ui| {
                ui.strong(if self.servers_visible {
                    "Servers"
                } else if self.mode == Mode::Query {
                    &self.documents[self.document_index].title
                } else {
                    self.table_tabs[self.table_index]
                        .selected_relation
                        .as_ref()
                        .map(|r| r.name.as_str())
                        .unwrap_or("Workspace")
                });
                ui.label(
                    RichText::new(
                        self.connected
                            .as_ref()
                            .map(|p| p.name.as_str())
                            .unwrap_or("PostgreSQL workspace"),
                    )
                    .small()
                    .color(theme::muted()),
                );
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.add_enabled_ui(!self.busy.control, |ui| {
                    if ui
                        .add_enabled(self.connected.is_some(), egui::Button::new("+ Query"))
                        .on_hover_text("New query (Command-T)")
                        .clicked()
                    {
                        self.new_document();
                        if self.connected.is_some() {
                            self.servers_visible = false;
                        }
                    }
                    if self.connected.is_some()
                        && ui
                            .add_enabled(!self.busy.any(), egui::Button::new("Disconnect"))
                            .clicked()
                        && !self.blocked_by_edits()
                    {
                        self.send(Command::Disconnect, "Disconnecting...");
                    }
                    if self.connected.is_some() {
                        if ui
                            .selectable_label(
                                self.mode == Mode::Roles && !self.servers_visible,
                                "Roles",
                            )
                            .clicked()
                        {
                            self.servers_visible = false;
                            self.open_roles();
                        }
                        if ui
                            .selectable_label(
                                self.mode == Mode::Activity && !self.servers_visible,
                                "Activity",
                            )
                            .on_hover_text("Sessions on this server")
                            .clicked()
                        {
                            self.servers_visible = false;
                            self.open_activity();
                        }
                        if ui
                            .selectable_label(
                                self.mode == Mode::Diagnostics && !self.servers_visible,
                                "Diagnostics",
                            )
                            .on_hover_text("Index advisor and slow queries")
                            .clicked()
                        {
                            self.servers_visible = false;
                            self.open_diagnostics();
                        }
                    }
                });
                ui.add_space(18.0);
                if let Some(profile) = &self.connected {
                    let mut badges = Vec::new();
                    if profile.read_only {
                        badges.push("read-only");
                    }
                    if profile.ssh.is_some() {
                        badges.push("SSH");
                    }
                    let label = if badges.is_empty() {
                        "Connected".to_owned()
                    } else {
                        format!("Connected ({})", badges.join(", "))
                    };
                    ui.label(RichText::new(label).color(theme::green()));
                    self.database_switcher(ui);
                    theme::database_icon(ui, 24.0, theme::muted());
                } else {
                    ui.label(RichText::new("Not connected").color(theme::muted()));
                }
            });
        });
    }

    pub(super) fn servers(&mut self, ui: &mut egui::Ui) {
        let previous_profile = self.draft.clone();
        let mut password_changed = false;
        let idle = !self.busy.control;
        egui::Panel::left("brand_rail")
            .exact_size(215.0)
            .frame(egui::Frame::new().fill(theme::toolbar()).inner_margin(20))
            .show(ui, |ui| {
                ui.add_space(34.0);
                ui.vertical_centered(|ui| {
                    match &self.logo {
                        Some(logo) => {
                            ui.add(
                                egui::Image::new(egui::load::SizedTexture::from_handle(logo))
                                    .fit_to_exact_size(egui::vec2(120.0, 120.0)),
                            );
                        }
                        None => theme::database_icon(ui, 100.0, theme::muted()),
                    }
                    ui.add_space(12.0);
                    ui.heading("Postern");
                    ui.label(
                        RichText::new("Connections, tables, and SQL.")
                            .small()
                            .color(theme::muted()),
                    );
                });
                ui.with_layout(egui::Layout::bottom_up(egui::Align::Min), |ui| {
                    ui.label(
                        RichText::new("PostgreSQL client")
                            .small()
                            .color(theme::muted()),
                    );
                    ui.add_space(12.0);
                    ui.add_enabled_ui(idle, |ui| {
                        if ui
                            .add_sized([175.0, 30.0], egui::Button::new("Export servers..."))
                            .on_hover_text(
                                "Save the server list as JSON. Passwords are not included.",
                            )
                            .clicked()
                        {
                            self.export_profiles();
                        }
                        if ui
                            .add_sized([175.0, 30.0], egui::Button::new("Import servers..."))
                            .clicked()
                        {
                            self.import_profiles();
                        }
                        ui.add_space(8.0);
                        if ui
                            .add_sized([175.0, 34.0], egui::Button::new("New Server from URL..."))
                            .clicked()
                        {
                            self.dialog = Some(Dialog::ImportUrl(String::new()));
                        }
                        if ui
                            .add_sized([175.0, 34.0], egui::Button::new("+ New Server"))
                            .clicked()
                        {
                            self.draft = ConnectionProfile::default();
                            self.password.clear();
                            self.remember_password = false;
                        }
                    });
                });
            });
        egui::Panel::left("server_list")
            .default_size(360.0)
            .min_size(220.0)
            .resizable(true)
            .frame(
                egui::Frame::new()
                    .fill(theme::background())
                    .inner_margin(18),
            )
            .show(ui, |ui| self.server_list(ui));
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(theme::sidebar()).inner_margin(28))
            .show(ui, |ui| {
                egui::Panel::bottom("connection_actions")
                    .exact_size(102.0)
                    .frame(egui::Frame::NONE)
                    .show(ui, |ui| self.connection_actions(ui));
                egui::ScrollArea::vertical().show(ui, |ui| {
                    ui.spacing_mut().item_spacing.y = 5.0;
                    ui.heading("Connection details");
                    ui.add_space(12.0);
                    ui.add_enabled_ui(idle, |ui| {
                        password_changed = self.connection_form(ui);
                    });
                });
            });
        if !previous_profile.same_endpoint(&self.draft) || password_changed {
            self.discovered_databases = None;
        }
    }

    fn server_list(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.strong("Saved servers");
            ui.label(RichText::new(self.profiles.len().to_string()).color(theme::muted()));
        });
        ui.add_space(6.0);
        ui.add(
            egui::TextEdit::singleline(&mut self.server_search)
                .hint_text("Filter servers")
                .desired_width(f32::INFINITY),
        );
        ui.add_space(12.0);
        let search = self.server_search.to_lowercase();
        let mut groups: Vec<&str> = self.profiles.iter().map(|p| p.group.trim()).collect();
        groups.sort_unstable();
        groups.dedup();
        let mut selected = None;
        let mut duplicate = None;
        egui::ScrollArea::vertical().show(ui, |ui| {
            ui.add_enabled_ui(!self.busy.control, |ui| {
                for group in groups {
                    let members: Vec<&ConnectionProfile> = self
                        .profiles
                        .iter()
                        .filter(|p| p.group.trim() == group)
                        .filter(|p| {
                            search.is_empty()
                                || p.name.to_lowercase().contains(&search)
                                || p.host.to_lowercase().contains(&search)
                                || p.database.to_lowercase().contains(&search)
                        })
                        .collect();
                    if members.is_empty() {
                        continue;
                    }
                    let show = |ui: &mut egui::Ui| {
                        for profile in members {
                            let chosen = self.draft.id == profile.id;
                            let response = egui::Frame::new()
                                .fill(if chosen {
                                    theme::selected()
                                } else {
                                    theme::sidebar()
                                })
                                .stroke(egui::Stroke::new(
                                    1.0,
                                    if chosen { theme::accent() } else { theme::border() },
                                ))
                                .corner_radius(7)
                                .inner_margin(12)
                                .show(ui, |ui| {
                                    ui.set_min_width(ui.available_width());
                                    ui.horizontal(|ui| {
                                        theme::database_icon(
                                            ui,
                                            29.0,
                                            theme::COLORS[profile.color % theme::COLORS.len()],
                                        );
                                        ui.vertical(|ui| {
                                            ui.strong(&profile.name);
                                            let via = if profile.ssh.is_some() { " via SSH" } else { "" };
                                            ui.label(
                                                RichText::new(format!(
                                                    "{} / {}{via}",
                                                    profile.host, profile.database
                                                ))
                                                .small()
                                                .color(theme::muted()),
                                            );
                                        });
                                    });
                                })
                                .response;
                            let response = ui.interact(
                                response.rect,
                                ui.scope_id().with(profile.id),
                                egui::Sense::click(),
                            );
                            if response.clicked() {
                                selected = Some(profile.clone());
                            }
                            response.context_menu(|ui| {
                                if ui.button("Duplicate").clicked() {
                                    duplicate = Some(profile.clone());
                                    ui.close();
                                }
                            });
                            ui.add_space(3.0);
                        }
                    };
                    if group.is_empty() {
                        show(ui);
                    } else {
                        egui::CollapsingHeader::new(RichText::new(group).strong())
                            .id_salt(("server_group", group))
                            .default_open(true)
                            .show(ui, show);
                    }
                }
                if self.profiles.is_empty() {
                    ui.add_space(20.0);
                    ui.label(
                        RichText::new(
                            "Your saved connections will appear here.\nAdd a server to get started.",
                        )
                        .color(theme::muted()),
                    );
                }
            });
        });
        if let Some(profile) = selected {
            self.password = match storage::password(&profile) {
                Ok(value) => value,
                Err(error) => {
                    self.error = Some(error.to_string());
                    String::new()
                }
            };
            self.remember_password = !self.password.is_empty();
            self.draft = profile;
        }
        if let Some(profile) = duplicate {
            self.draft = ConnectionProfile {
                id: Uuid::new_v4(),
                name: format!("{} copy", profile.name),
                ..profile
            };
            self.password.clear();
            self.remember_password = false;
            self.status = "Duplicated. Save to keep the copy.".into();
        }
    }

    fn connection_actions(&mut self, ui: &mut egui::Ui) {
        ui.add_enabled_ui(!self.busy.control, |ui| {
            ui.separator();
            ui.add_space(8.0);
            ui.horizontal_wrapped(|ui| {
                if ui.button("Save").clicked() {
                    match self.save_profile() {
                        Ok(()) => self.status = "Server saved".into(),
                        Err(e) => self.error = Some(e.to_string()),
                    }
                }
                if ui.button("Test").clicked() {
                    self.send(
                        Command::Test(self.draft.clone(), self.password.clone()),
                        "Testing connection...",
                    );
                }
                if ui.button("Show Databases").clicked() {
                    self.discovered_databases = None;
                    self.send(
                        Command::DiscoverDatabases(self.draft.clone(), self.password.clone()),
                        "Looking up databases...",
                    );
                }
                if theme::primary(ui, "Connect").clicked() {
                    match self.save_profile() {
                        Ok(()) => {
                            self.send(
                                Command::Connect(self.draft.clone(), self.password.clone()),
                                "Connecting...",
                            );
                        }
                        Err(error) => self.error = Some(error.to_string()),
                    }
                }
            });
            ui.add_space(16.0);
            if self.profiles.iter().any(|p| p.id == self.draft.id)
                && ui.small_button("Remove saved server").clicked()
            {
                let remaining: Vec<_> = self
                    .profiles
                    .iter()
                    .filter(|p| p.id != self.draft.id)
                    .cloned()
                    .collect();
                let result = self
                    .store
                    .as_ref()
                    .context("Connection storage unavailable.")
                    .and_then(|store| store.save_profiles(&remaining));
                match result {
                    Ok(()) => {
                        self.profiles = remaining;
                        if let Err(e) = storage::save_password(&self.draft, "") {
                            self.error = Some(e.to_string());
                        }
                        self.draft = ConnectionProfile::default();
                        self.password.clear();
                    }
                    Err(e) => self.error = Some(e.to_string()),
                }
            }
        });
    }

    /// Renders the profile form. Returns true when the password changed.
    fn connection_form(&mut self, ui: &mut egui::Ui) -> bool {
        let mut password_changed = false;
        field(ui, "Nickname", &mut self.draft.name, "Local PostgreSQL");
        field(
            ui,
            "Group",
            &mut self.draft.group,
            "Optional folder, e.g. Production",
        );
        ui.horizontal(|ui| {
            ui.label(RichText::new("Color").color(theme::muted()));
            for (index, color) in theme::COLORS.iter().enumerate() {
                let (rect, response) =
                    ui.allocate_exact_size(egui::vec2(23.0, 23.0), egui::Sense::click());
                ui.painter().circle_filled(rect.center(), 7.0, *color);
                if self.draft.color == index {
                    ui.painter().circle_stroke(
                        rect.center(),
                        10.0,
                        egui::Stroke::new(1.0, egui::Color32::WHITE),
                    );
                }
                if response.clicked() {
                    self.draft.color = index;
                }
            }
        });
        section(ui, "Server");
        field(
            ui,
            "Host",
            &mut self.draft.host,
            "localhost or /tmp for a socket",
        );
        ui.horizontal(|ui| {
            ui.label(RichText::new("Port").color(theme::muted()));
            ui.add(
                egui::DragValue::new(&mut self.draft.port)
                    .range(1..=65535)
                    .speed(1),
            );
        });
        field(ui, "Database", &mut self.draft.database, "postgres");
        if let Some(databases) = &self.discovered_databases {
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(format!("{} available", databases.len()))
                        .small()
                        .color(theme::muted()),
                );
                #[cfg(feature = "smoke")]
                Self::open_smoke_combo(
                    self.smoke.as_ref(),
                    ui,
                    "discovered_databases",
                    "discovery",
                );
                egui::ComboBox::from_id_salt("discovered_databases")
                    .selected_text("Choose a database...")
                    .width(240.0)
                    .height(300.0)
                    .show_ui(ui, |ui| {
                        for name in databases {
                            ui.selectable_value(&mut self.draft.database, name.clone(), name);
                        }
                        if databases.is_empty() {
                            ui.label("No accessible databases found.");
                        }
                    });
            });
        } else {
            ui.label(
                RichText::new("Use Show Databases to choose a database on this server.")
                    .small()
                    .color(theme::muted()),
            );
        }
        field(ui, "User", &mut self.draft.username, "postgres");
        ui.label(RichText::new("Password").small().color(theme::muted()));
        password_changed |= ui
            .add(
                egui::TextEdit::singleline(&mut self.password)
                    .password(true)
                    .hint_text("Optional. Empty uses ~/.pgpass when present.")
                    .desired_width(f32::INFINITY),
            )
            .changed();
        ui.checkbox(
            &mut self.remember_password,
            "Save password in macOS Keychain",
        );
        if ui.small_button("Load saved password").clicked() {
            match storage::password(&self.draft) {
                Ok(value) => {
                    self.password = value;
                    self.remember_password = true;
                    password_changed = true;
                }
                Err(error) => self.error = Some(error.to_string()),
            }
        }
        section(ui, "Security");
        ui.horizontal(|ui| {
            ui.label("SSL mode");
            egui::ComboBox::from_id_salt("ssl")
                .selected_text(self.draft.ssl.label())
                .show_ui(ui, |ui| {
                    for mode in SslMode::ALL {
                        ui.selectable_value(&mut self.draft.ssl, mode, mode.label());
                    }
                });
        });
        ui.label(
            RichText::new(match self.draft.ssl {
                SslMode::Disable => "Unencrypted connection.",
                SslMode::Prefer => {
                    "Use encryption when offered. The server certificate is not verified."
                }
                SslMode::Require => "Require encryption. The server certificate is not verified.",
                SslMode::VerifyCa => {
                    "Require encryption and verify the certificate chain, but not the host name."
                }
                SslMode::VerifyFull => {
                    "Require encryption and verify the certificate and host name."
                }
            })
            .small()
            .color(theme::muted()),
        );
        if self.draft.ssl != SslMode::Disable {
            file_field(
                ui,
                "CA file",
                &mut self.draft.ssl_root_cert,
                "Optional PEM bundle added to system trust",
            );
            file_field(
                ui,
                "Client cert",
                &mut self.draft.ssl_client_cert,
                "Optional PEM certificate",
            );
            file_field(
                ui,
                "Client key",
                &mut self.draft.ssl_client_key,
                "Unencrypted PKCS#8 PEM key",
            );
        }
        section(ui, "SSH tunnel");
        let mut tunnel = self.draft.ssh.is_some();
        if ui
            .checkbox(&mut tunnel, "Connect through an SSH tunnel")
            .changed()
        {
            self.draft.ssh = tunnel.then(SshTunnelConfig::default);
        }
        if let Some(ssh) = &mut self.draft.ssh {
            field(ui, "SSH host", &mut ssh.host, "bastion.example.com");
            ui.horizontal(|ui| {
                ui.label(RichText::new("SSH port").color(theme::muted()));
                ui.add(
                    egui::DragValue::new(&mut ssh.port)
                        .range(1..=65535)
                        .speed(1),
                );
            });
            field(
                ui,
                "SSH user",
                &mut ssh.user,
                "Optional, defaults to ~/.ssh/config",
            );
            file_field(
                ui,
                "Identity",
                &mut ssh.identity_file,
                "Optional key file; the SSH agent is used otherwise",
            );
            ui.checkbox(
                &mut ssh.accept_new_host_key,
                "Trust the host key on first connection",
            )
            .on_hover_text(
                "Adds an unknown host key to ~/.ssh/known_hosts. A changed key is always rejected.",
            );
            ui.label(
                RichText::new("The database host and port are resolved on the SSH server. Password and keyboard-interactive SSH logins are not supported; use a key or the SSH agent.")
                    .small()
                    .color(theme::muted()),
            );
        }
        section(ui, "Session");
        ui.checkbox(&mut self.draft.read_only, "Read-only connection")
            .on_hover_text(
                "Sets default_transaction_read_only for the table browser and the SQL editor.",
            );
        ui.horizontal(|ui| {
            ui.label(RichText::new("SQL statement timeout").color(theme::muted()));
            ui.add(
                egui::DragValue::new(&mut self.draft.statement_timeout_secs)
                    .range(0..=86_400)
                    .suffix(" s"),
            )
            .on_hover_text("Applies to the SQL editor. 0 disables the timeout. The table browser always uses 30 seconds.");
        });
        field(
            ui,
            "Options",
            &mut self.draft.connection_options,
            "Optional libpq options, e.g. -c search_path=app",
        );
        ui.label(RichText::new("Startup SQL").small().color(theme::muted()));
        ui.add(
            egui::TextEdit::multiline(&mut self.draft.startup_sql)
                .code_editor()
                .desired_rows(3)
                .desired_width(f32::INFINITY)
                .hint_text("Runs after each session connects, e.g. SET search_path = app, public;"),
        );
        password_changed
    }

    fn database_switcher(&mut self, ui: &mut egui::Ui) {
        let current = self
            .connected
            .as_ref()
            .map(|p| p.database.clone())
            .unwrap_or_default();
        let mut chosen = None;
        let mut refresh = false;
        let mut create = false;
        let mut drop = None;
        ui.add_enabled_ui(
            !self.busy.any() && self.dialog.is_none() && self.error.is_none(),
            |ui| {
                #[cfg(feature = "smoke")]
                Self::open_smoke_combo(self.smoke.as_ref(), ui, "current_database", "databases");
                egui::ComboBox::from_id_salt("current_database")
                    .selected_text(&current)
                    .width(240.0)
                    .height(340.0)
                    .show_ui(ui, |ui| {
                        ui.label(
                            RichText::new("Databases on this server")
                                .small()
                                .color(theme::muted()),
                        );
                        for database in &self.databases {
                            if ui
                                .selectable_label(database == &current, database)
                                .clicked()
                            {
                                chosen = Some(database.clone());
                                ui.close();
                            }
                        }
                        if self.databases.is_empty() {
                            ui.label("No databases loaded.");
                        }
                        ui.separator();
                        if ui.button("Refresh databases").clicked() {
                            refresh = true;
                            ui.close();
                        }
                        if ui.button("New database...").clicked() {
                            create = true;
                            ui.close();
                        }
                        ui.menu_button("Drop database", |ui| {
                            for database in self.databases.iter().filter(|d| **d != current) {
                                if ui.button(database).clicked() {
                                    drop = Some(database.clone());
                                    ui.close();
                                }
                            }
                        });
                    });
            },
        );
        if let Some(database) = chosen
            && database != current
        {
            self.send(Command::SwitchDatabase(database), "Switching database...");
        } else if refresh {
            self.send(Command::Databases, "Refreshing databases...");
        } else if create {
            self.dialog = Some(Dialog::CreateDatabase(String::new()));
        } else if let Some(name) = drop {
            self.dialog = Some(Dialog::DropDatabase {
                name,
                confirmation: String::new(),
            });
        }
    }

    fn save_profile(&mut self) -> Result<()> {
        self.draft.validate()?;
        anyhow::ensure!(
            self.profiles_writable,
            "Connection storage is unavailable or corrupt. Repair it before saving."
        );
        let mut profiles = self.profiles.clone();
        if let Some(profile) = profiles.iter_mut().find(|p| p.id == self.draft.id) {
            *profile = self.draft.clone();
        } else {
            profiles.push(self.draft.clone());
        }
        self.store
            .as_ref()
            .context("Connection storage unavailable.")?
            .save_profiles(&profiles)?;
        self.profiles = profiles;
        storage::save_password(
            &self.draft,
            if self.remember_password {
                &self.password
            } else {
                ""
            },
        )?;
        Ok(())
    }

    fn export_profiles(&mut self) {
        let Some(path) = rfd::FileDialog::new()
            .add_filter("JSON", &["json"])
            .set_file_name("postern-servers.json")
            .save_file()
        else {
            return;
        };
        let result = serde_json::to_vec_pretty(&self.profiles)
            .map_err(anyhow::Error::from)
            .and_then(|bytes| storage::write_user_file(&path, &bytes));
        match result {
            Ok(()) => self.status = format!("Exported {} servers", self.profiles.len()),
            Err(error) => self.error = Some(error.to_string()),
        }
    }

    /// Adds servers from an exported JSON file. Servers whose ID already exists
    /// are imported as copies with a new ID, so nothing is overwritten.
    fn import_profiles(&mut self) {
        let Some(path) = rfd::FileDialog::new()
            .add_filter("JSON", &["json"])
            .pick_file()
        else {
            return;
        };
        let result = (|| -> Result<usize> {
            anyhow::ensure!(
                self.profiles_writable,
                "Connection storage is unavailable or corrupt. Repair it before importing."
            );
            let bytes = std::fs::read(&path)?;
            let imported: Vec<ConnectionProfile> =
                serde_json::from_slice(&bytes).context("The file is not a Postern server list.")?;
            let mut profiles = self.profiles.clone();
            for mut profile in imported {
                profile.validate()?;
                if profiles.iter().any(|p| p.id == profile.id) {
                    profile.id = Uuid::new_v4();
                }
                profiles.push(profile);
            }
            let count = profiles.len() - self.profiles.len();
            self.store
                .as_ref()
                .context("Connection storage unavailable.")?
                .save_profiles(&profiles)?;
            self.profiles = profiles;
            Ok(count)
        })();
        match result {
            Ok(count) => self.status = format!("Imported {count} servers"),
            Err(error) => self.error = Some(error.to_string()),
        }
    }
}

fn section(ui: &mut egui::Ui, title: &str) {
    ui.add_space(14.0);
    ui.separator();
    ui.add_space(6.0);
    ui.label(RichText::new(title).strong());
    ui.add_space(4.0);
}

fn field(ui: &mut egui::Ui, label: &str, value: &mut String, placeholder: &str) {
    ui.horizontal(|ui| {
        ui.add_sized(
            [85.0, 28.0],
            egui::Label::new(RichText::new(label).color(theme::muted())),
        );
        ui.add(
            egui::TextEdit::singleline(value)
                .hint_text(placeholder)
                .desired_width(f32::INFINITY),
        );
    });
}

fn file_field(ui: &mut egui::Ui, label: &str, value: &mut String, placeholder: &str) {
    ui.horizontal(|ui| {
        ui.add_sized(
            [85.0, 28.0],
            egui::Label::new(RichText::new(label).color(theme::muted())),
        );
        if ui.button("Choose...").clicked()
            && let Some(path) = rfd::FileDialog::new().pick_file()
        {
            *value = path.to_string_lossy().into_owned();
        }
        ui.add(
            egui::TextEdit::singleline(value)
                .hint_text(placeholder)
                .desired_width(f32::INFINITY),
        );
    });
}
