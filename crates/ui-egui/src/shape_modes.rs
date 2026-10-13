//! Shape-tool modes, path actions and view-only path display options.
use crate::{PhotocraftApp, state::Tool};
use photocraft_doc::{LayerContent, LiveShape, Path, ShapeLayer};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct ShapeToolOptions {
    pub mode: String,
    pub operation: String,
    pub blend: photocraft_color::BlendMode,
    pub opacity: f32,
    pub anti_alias: bool,
    pub path_width: f32,
    pub path_color: Option<[u8; 3]>,
    pub show_path: bool,
    /// Show resize/rotate controls automatically on the selected shape layer.
    pub show_transform: bool,
    pub from_center: bool,
    pub constrain: bool,
    pub size_mode: String,
    pub size_width: f64,
    pub size_height: f64,
    pub align_edges: bool,
    pub star_ratio: f64,
    pub smooth_indents: bool,
    pub align_canvas: bool,
    pub arrowheads: photocraft_doc::vector::LineArrowheads,
    #[serde(skip)]
    pub path_geometry: Option<(u64, Value, Path)>,
}
impl Default for ShapeToolOptions {
    fn default() -> Self {
        Self {
            mode: "shape".into(),
            operation: "new".into(),
            blend: photocraft_color::BlendMode::Normal,
            opacity: 100.0,
            anti_alias: true,
            path_width: 1.0,
            path_color: None,
            show_path: true,
            show_transform: true,
            from_center: false,
            constrain: false,
            size_mode: "unconstrained".into(),
            size_width: 100.0,
            size_height: 100.0,
            align_edges: true,
            star_ratio: 1.0,
            smooth_indents: false,
            align_canvas: false,
            arrowheads: photocraft_doc::vector::LineArrowheads { width: 10.0, length: 20.0, ..Default::default() },
            path_geometry: None,
        }
    }
}
impl ShapeToolOptions {
    pub(crate) fn validate(&self) -> Result<(), String> {
        if !matches!(self.mode.as_str(), "shape" | "path" | "pixels") {
            return Err("shapeOptions.mode must be shape, path or pixels".into());
        }
        if !matches!(self.operation.as_str(), "new" | "combine" | "subtract" | "intersect" | "exclude") {
            return Err("Invalid shapeOptions.operation".into());
        }
        if !matches!(self.size_mode.as_str(), "unconstrained" | "square" | "fixed" | "proportional") {
            return Err("Invalid shapeOptions.sizeMode".into());
        }
        if !(0.0..=100.0).contains(&self.opacity)
            || !(0.5..=10.0).contains(&self.path_width)
            || ![self.arrowheads.width, self.arrowheads.length].iter().all(|v| (0.0..=10000.0).contains(v))
            || !(-50.0..=50.0).contains(&self.arrowheads.concavity)
            || ![self.size_width, self.size_height].iter().all(|v| (1.0..=300000.0).contains(v))
            || !(0.01..=1.0).contains(&self.star_ratio)
        {
            return Err("Shape options contain invalid numeric values".into());
        }
        Ok(())
    }
}

pub(crate) fn path_shape(app: &PhotocraftApp) -> Option<ShapeLayer> {
    let st = app.session.active()?;
    let name = app.ui.selected_path.as_deref().filter(|n| *n != "layer").unwrap_or("work");
    let path = if name == "work" { st.doc.work_path.as_ref()? } else { &st.doc.paths.iter().find(|p| p.name == name)?.path };
    let live = app.ui.tool_options.shape.path_geometry.as_ref().and_then(|(doc, geometry, original)| {
        if *doc == st.doc.id.0 && name == "work" && original == path { photocraft_engine::vector_cmds::shape_live(geometry).ok() } else { None }
    });
    Some(ShapeLayer { path: path.clone(), live, ..Default::default() })
}

pub(crate) fn edit_path(app: &mut PhotocraftApp, patch: Value) -> Result<(), String> {
    let shape = path_shape(app).ok_or("No path to edit")?;
    let name = app.ui.selected_path.as_deref().filter(|n| *n != "layer").unwrap_or("work").to_string();
    let mut geometry = app
        .ui
        .tool_options
        .shape
        .path_geometry
        .as_ref()
        .filter(|(doc, _, original)| shape.live.is_some() && original == &shape.path && app.session.active().is_some_and(|st| st.doc.id.0 == *doc))
        .map(|(_, p, _)| p.clone());
    let path = if patch.get("transform").is_some() {
        let m: [f64; 6] = serde_json::from_value(patch["transform"].clone()).map_err(|e| e.to_string())?;
        geometry = None;
        shape.path.transform(&photocraft_geom::Affine { m })
    } else {
        let p = geometry.as_mut().ok_or("This path has no live shape parameters")?;
        if let Some(patch) = patch.as_object() {
            for (k, v) in patch {
                p[k] = v.clone();
            }
        }
        photocraft_engine::vector_cmds::shape_path(p).map_err(|e| e.to_string())?
    };
    photocraft_vector::flatten::validate_shape(&path, None).map_err(String::from)?;
    let mut params = json!({"name": name, "path": photocraft_engine::vector_cmds::path_json(&path)});
    if let Some(key) = patch.get("coalesce") {
        params["coalesce"] = key.clone();
    }
    app.run("path.set", params)?;
    app.ui.tool_options.shape.path_geometry = geometry.and_then(|p| app.session.active().map(|st| (st.doc.id.0, p, path)));
    Ok(())
}

pub(crate) fn create(app: &mut PhotocraftApp, geometry: Value) -> Result<Value, String> {
    create_with_operation(app, geometry, None)
}

pub(crate) fn create_with_operation(app: &mut PhotocraftApp, mut geometry: Value, operation: Option<&str>) -> Result<Value, String> {
    app.ui.tool_options.shape.validate()?;
    match app.ui.tool_options.shape.mode.as_str() {
        "path" => {
            let path = if geometry["kind"] == "path" {
                photocraft_engine::vector_cmds::parse_path(&geometry["path"]).map_err(|e| e.to_string())?
            } else {
                photocraft_engine::vector_cmds::shape_path(&geometry).map_err(|e| e.to_string())?
            };
            photocraft_vector::flatten::validate_shape(&path, None).map_err(String::from)?;
            let mut params = json!({"name": "work", "path": photocraft_engine::vector_cmds::path_json(&path)});
            let op = operation.unwrap_or(&app.ui.tool_options.shape.operation).to_string();
            if op != "new" {
                if let Some(old) = app.session.active().and_then(|st| st.doc.work_path.as_ref()) {
                    let mut combined = old.clone();
                    combined.subpaths.extend(path.subpaths.iter().cloned());
                    photocraft_vector::flatten::validate_shape(&combined, None).map_err(String::from)?;
                }
                params["op"] = json!(op);
            }
            let result = app.run("path.set", params)?;
            let original = app.session.active().and_then(|st| st.doc.work_path.clone());
            app.ui.tool_options.shape.path_geometry =
                if op == "new" { original.and_then(|p| app.session.active().map(|st| (st.doc.id.0, geometry, p))) } else { None };
            app.ui.selected_path = Some("work".into());
            Ok(result)
        }
        "pixels" => {
            geometry["mode"] = json!(app.ui.tool_options.shape.blend);
            geometry["opacity"] = json!(app.ui.tool_options.shape.opacity);
            geometry["antiAlias"] = json!(app.ui.tool_options.shape.anti_alias);
            geometry["color"] = json!(app.session.tools.foreground);
            geometry["target"] = crate::canvas::paint_target(app);
            app.run("shape.drawPixels", geometry)
        }
        _ => {
            let op = operation.unwrap_or(&app.ui.tool_options.shape.operation).to_string();
            if op != "new" {
                let id = app
                    .session
                    .active()
                    .and_then(|st| st.active_layer)
                    .filter(|id| app.session.active().and_then(|st| st.doc.layer(*id)).is_some_and(|l| matches!(l.content, LayerContent::Shape(_))))
                    .ok_or("Select a shape layer for this operation")?;
                geometry["addTo"] = json!(id.0);
                geometry["op"] = json!(op);
            }
            app.run("shape.create", geometry)
        }
    }
}

pub(crate) fn path_display(app: &PhotocraftApp, ctx: &egui::Context) -> (egui::Color32, f32) {
    let o = &app.ui.tool_options.shape;
    let color = o.path_color.map_or(crate::theme::Tokens::get(ctx).accent, |c| egui::Color32::from_rgb(c[0], c[1], c[2]));
    (color, o.path_width.clamp(0.5, 10.0))
}

fn run(app: &mut PhotocraftApp, id: &str, p: Value) {
    if let Err(e) = app.run(id, p) {
        app.ui.status = e;
        app.ui.status_error = true;
    }
}

fn menu_button(ui: &mut egui::Ui, icon: &str, label: &str) -> egui::Response {
    let r = crate::icons::button(ui, icon, 24.0, false, label);
    r.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, ui.is_enabled(), label));
    r
}

pub(crate) fn actions(app: &mut PhotocraftApp, ui: &mut egui::Ui, tool: Tool) {
    let mode = app.ui.tool_options.shape.mode.clone();
    if mode == "pixels" {
        ui.label(tl!("Mode:"));
        let opts: Vec<_> = photocraft_color::BlendMode::LAYER_MODES.into_iter().map(|m| (m, m.label())).collect();
        crate::widgets::dropdown(ui, "shape-pixel-blend", &mut app.ui.tool_options.shape.blend, &opts, 126.0);
        ui.label(tl!("Opacity:"));
        crate::widgets::value_field(ui, &mut app.ui.tool_options.shape.opacity, 0.0..=100.0, "%", 58.0);
        ui.checkbox(&mut app.ui.tool_options.shape.anti_alias, tl!("Anti-alias"));
    } else {
        if mode == "path" {
            let name = app.ui.selected_path.clone().filter(|n| n != "layer").unwrap_or("work".into());
            let path = path_shape(app);
            ui.add_enabled_ui(path.is_some(), |ui| {
                if ui.button(tl!("Selection")).clicked() {
                    run(app, "path.toSelection", json!({"name":name}));
                }
                let mask =
                    app.session.active().and_then(|st| st.active_layer.and_then(|id| st.doc.layer(id))).is_some_and(|l| {
                        !matches!(l.content, LayerContent::Shape(_)) && !app.session.active().is_some_and(|st| st.doc.effective_locks(l.id).all)
                    });
                if ui.add_enabled(mask, egui::Button::new(tl!("Mask"))).clicked() {
                    run(app, "layer.vectorMask.fromPath", json!({"name":name}));
                }
                if ui.button(tl!("Shape")).clicked()
                    && let Some(path) = &path
                {
                    let geometry = json!({"kind":"path","path":photocraft_engine::vector_cmds::path_json(&path.path),"fill":app.session.tools.foreground});
                    match app.run("shape.create", geometry) {
                        Ok(_) => {
                            app.ui.selected_path = Some("layer".into());
                            app.ui.tool_options.shape.mode = "shape".into();
                        }
                        Err(e) => {
                            app.ui.status = e;
                            app.ui.status_error = true;
                        }
                    }
                }
            });
        }
        operation_menu(app, ui);
        component_menus(app, ui);
    }
    options_menu(app, ui, tool);
    ui.checkbox(&mut app.ui.tool_options.shape.align_edges, tl!("Align Edges"));
}

fn operation_menu(app: &mut PhotocraftApp, ui: &mut egui::Ui) {
    let shape_mode = app.ui.tool_options.shape.mode == "shape";
    let active = (!app.ui.mask_target && !app.ui.vector_mask_target).then(|| crate::vector_ui::active_shape_path(app)).flatten();
    let unlocked = app.session.active().is_some_and(|st| {
        st.active_layer.is_some_and(|id| {
            let l = st.doc.effective_locks(id);
            !l.all && !l.pixels && !l.position
        })
    });
    let response = menu_button(ui, "square-plus", tl!("Path Operations"));
    egui::Popup::menu(&response).show(|ui| {
        for (id, label) in [
            ("new", if shape_mode { tl!("New Layer") } else { tl!("New Path") }),
            ("combine", tl!("Combine Shapes")),
            ("subtract", tl!("Subtract Front Shape")),
            ("intersect", tl!("Intersect Shape Areas")),
            ("exclude", tl!("Exclude Overlapping Shapes")),
        ] {
            let enabled = id == "new" || if shape_mode { active.is_some() && unlocked } else { path_shape(app).is_some_and(|s| !s.path.subpaths.is_empty()) };
            if ui.add_enabled(enabled, egui::Button::selectable(app.ui.tool_options.shape.operation == id, label)).clicked() {
                app.ui.tool_options.shape.operation = id.into();
                ui.close();
            }
        }
        let merge = if shape_mode && !app.ui.mask_target && !app.ui.vector_mask_target {
            let selected = photocraft_engine::layer_multi_cmds::selected(&app.session);
            if selected.len() > 1 {
                app.session.active().is_some_and(|st| {
                    selected.iter().all(|id| {
                        let locks = st.doc.effective_locks(*id);
                        !locks.all
                            && !locks.pixels
                            && !locks.position
                            && st.doc.layer(*id).is_some_and(|l| matches!(l.content, photocraft_doc::LayerContent::Shape(_)))
                    })
                })
            } else {
                active.as_ref().is_some_and(|(_, p)| p.components().len() > 1) && unlocked
            }
        } else if !shape_mode {
            path_shape(app).is_some_and(|s| s.path.components().len() > 1)
        } else {
            false
        };
        if ui.add_enabled(merge, egui::Button::new(tl!("Merge Shape Components"))).clicked() {
            if shape_mode {
                run(app, "layer.combineShapes.mergeShapeComponents", json!({}));
            } else {
                run(app, "path.merge", json!({"name":app.ui.selected_path.as_deref().filter(|n| *n != "layer").unwrap_or("work")}));
            }
            ui.close();
        }
    });
}

fn component_menus(app: &mut PhotocraftApp, ui: &mut egui::Ui) {
    let path_mode = app.ui.tool_options.shape.mode == "path";
    let (mut params, path) = if path_mode {
        (json!({"name":app.ui.selected_path.as_deref().filter(|n| *n != "layer").unwrap_or("work")}), path_shape(app).map(|s| s.path))
    } else if app.ui.mask_target || app.ui.vector_mask_target {
        (json!({}), None)
    } else {
        match crate::vector_ui::active_shape_path(app) {
            Some((id, p)) => (json!({"name":"layer","layer":id}), Some(p)),
            None => (json!({}), None),
        }
    };
    let count = path.as_ref().map_or(0, |p| p.components().len());
    let selected = path
        .as_ref()
        .map(|p| {
            let ds = &app.ui.direct_selection;
            let target_matches = if path_mode {
                ds.target == Some(crate::direct_select::PathRef::Work) && params["name"] == "work"
            } else {
                ds.target == params["layer"].as_u64().map(crate::direct_select::PathRef::Layer)
            };
            if !target_matches {
                return Vec::new();
            }
            p.components().into_iter().enumerate().filter(|(_, r)| ds.anchors.iter().any(|a| r.contains(&a[0]))).map(|(i, _)| i).collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let locked = params["layer"].as_u64().is_some_and(|id| {
        app.session.active().is_some_and(|st| {
            let l = st.doc.effective_locks(photocraft_doc::LayerId(id));
            l.all || l.pixels
        })
    });
    let position_locked =
        params["layer"].as_u64().is_some_and(|id| app.session.active().is_some_and(|st| st.doc.effective_locks(photocraft_doc::LayerId(id)).position));
    ui.add_enabled_ui(count > 0 && !locked, |ui| {
        let response = menu_button(ui, "align-left", tl!("Path Alignment"));
        egui::Popup::menu(&response).show(|ui| {
            ui.checkbox(&mut app.ui.tool_options.shape.align_canvas, tl!("Align to Canvas"));
            let n = if selected.is_empty() { count } else { selected.len() };
            for (action, label) in [
                ("left", tl!("Left")),
                ("centerX", tl!("Horizontal Centers")),
                ("right", tl!("Right")),
                ("top", tl!("Top")),
                ("centerY", tl!("Vertical Centers")),
                ("bottom", tl!("Bottom")),
                ("distributeX", tl!("Distribute Horizontally")),
                ("distributeY", tl!("Distribute Vertically")),
            ] {
                let enabled = if action.starts_with("distribute") { n >= 3 } else { n >= 2 || app.ui.tool_options.shape.align_canvas };
                if ui.add_enabled(enabled && !position_locked, egui::Button::new(label)).clicked() {
                    params["action"] = json!(action);
                    params["to"] = json!(if app.ui.tool_options.shape.align_canvas { "canvas" } else { "selection" });
                    if !selected.is_empty() {
                        params["components"] = json!(selected);
                    }
                    run(app, "path.align", params.clone());
                    ui.close();
                }
            }
        });
        ui.add_enabled_ui(count > 1, |ui| {
            let response = menu_button(ui, "layers", tl!("Path Arrangement"));
            egui::Popup::menu(&response).show(|ui| {
                let selected = if selected.is_empty() { vec![count.saturating_sub(1)] } else { selected.clone() };
                for (action, label) in [
                    ("front", tl!("Bring Shape to Front")),
                    ("forward", tl!("Bring Shape Forward")),
                    ("backward", tl!("Send Shape Backward")),
                    ("back", tl!("Send Shape to Back")),
                ] {
                    let toward_front = matches!(action, "front" | "forward");
                    let enabled = if toward_front {
                        selected.iter().any(|i| (*i + 1..count).any(|j| !selected.contains(&j)))
                    } else {
                        selected.iter().any(|i| (0..*i).any(|j| !selected.contains(&j)))
                    };
                    if ui.add_enabled(enabled, egui::Button::new(label)).clicked() {
                        params["action"] = json!(action);
                        params["components"] = json!(selected);
                        run(app, "path.arrange", params.clone());
                        app.ui.direct_selection = Default::default();
                        ui.close();
                    }
                }
            });
        });
    });
}

fn options_menu(app: &mut PhotocraftApp, ui: &mut egui::Ui, tool: Tool) {
    let shape = if app.ui.tool_options.shape.mode == "path" {
        path_shape(app)
    } else if app.ui.tool_options.shape.mode == "shape" && !app.ui.mask_target && !app.ui.vector_mask_target {
        crate::vector_ui::active_shape_path(app).and_then(|(id, _)| app.session.active()?.doc.layer(photocraft_doc::LayerId(id))).and_then(|l| {
            match &l.content {
                LayerContent::Shape(sh) => Some(sh.clone()),
                _ => None,
            }
        })
    } else {
        None
    };
    let live_heads = shape.as_ref().and_then(|s| match &s.live {
        Some(LiveShape::Line { arrowheads, .. }) => Some(arrowheads.clone().unwrap_or_else(|| photocraft_doc::vector::LineArrowheads {
            start: false,
            end: false,
            ..app.ui.tool_options.shape.arrowheads.clone()
        })),
        _ => None,
    });
    let mut heads = live_heads.clone().unwrap_or_else(|| app.ui.tool_options.shape.arrowheads.clone());
    let before = heads.clone();
    let locked = app.ui.tool_options.shape.mode == "shape"
        && app.session.active().is_some_and(|st| {
            st.active_layer.is_some_and(|id| {
                let l = st.doc.effective_locks(id);
                l.all || l.pixels
            })
        });
    let response = menu_button(ui, "settings", tl!("Path Options"));
    egui::Popup::menu(&response).close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside).show(|ui| {
        let o = &mut app.ui.tool_options.shape;
        ui.checkbox(&mut o.show_path, tl!("Show Path"));
        ui.checkbox(&mut o.show_transform, tl!("Show Transform Controls"));
        ui.horizontal(|ui| {
            ui.label(tl!("Width:"));
            crate::widgets::value_field(ui, &mut o.path_width, 0.5..=10.0, "px", 62.0);
        });
        let mut color = o.path_color.unwrap_or_else(|| {
            let c = crate::theme::Tokens::get(ui.ctx()).accent;
            [c.r(), c.g(), c.b()]
        });
        ui.horizontal(|ui| {
            ui.label(tl!("Color:"));
            if ui.color_edit_button_srgb(&mut color).changed() {
                o.path_color = Some(color);
            }
        });
        if ui.button(tl!("Reset")).clicked() {
            o.path_color = None;
            o.path_width = 1.0;
            o.show_path = true;
            o.show_transform = true;
        }
        ui.separator();
        ui.checkbox(&mut o.from_center, tl!("From Center"));
        ui.checkbox(&mut o.constrain, tl!("Constrain Proportions"));
        if tool != Tool::Line {
            for (mode, label) in
                [("unconstrained", tl!("Unconstrained")), ("square", tl!("Square")), ("fixed", tl!("Fixed Size")), ("proportional", tl!("Proportional"))]
            {
                ui.radio_value(&mut o.size_mode, mode.into(), label);
            }
            if matches!(o.size_mode.as_str(), "fixed" | "proportional") {
                for (label, value) in [(tl!("Width:"), &mut o.size_width), (tl!("Height:"), &mut o.size_height)] {
                    ui.horizontal(|ui| {
                        ui.label(label);
                        let mut number = *value as f32;
                        if crate::widgets::value_field(ui, &mut number, 1.0..=300000.0, "px", 70.0).changed() {
                            *value = f64::from(number);
                        }
                    });
                }
            }
        }
        if tool == Tool::Line {
            ui.separator();
            ui.add_enabled_ui(!locked && (shape.is_none() || live_heads.is_some() || o.mode == "pixels"), |ui| {
                ui.label(tl!("Arrowheads"));
                ui.horizontal(|ui| {
                    ui.checkbox(&mut heads.start, tl!("Start"));
                    ui.checkbox(&mut heads.end, tl!("End"));
                });
                for (label, value) in [(tl!("Width:"), &mut heads.width), (tl!("Length:"), &mut heads.length)] {
                    let mut number = *value as f32;
                    ui.horizontal(|ui| {
                        ui.label(label);
                        if crate::widgets::value_field(ui, &mut number, 0.0..=10000.0, "px", 70.0).changed() {
                            *value = f64::from(number);
                        }
                    });
                }
                ui.horizontal(|ui| {
                    ui.label(tl!("Concavity:"));
                    let mut number = heads.concavity as f32;
                    if crate::widgets::value_field(ui, &mut number, -50.0..=50.0, "%", 70.0).changed() {
                        heads.concavity = f64::from(number);
                    }
                });
            });
        }
    });
    if heads != before {
        let mut params = json!({"arrowheads":heads});
        if let Some(key) = crate::shape_options::edit_key(ui.ctx()) {
            params["coalesce"] = json!(format!("shape-arrowheads-{:?}-{key}", app.session.active().map(|st| (st.doc.id, st.active_layer))));
        }
        let result = if app.ui.tool_options.shape.mode == "path" && live_heads.is_some() {
            edit_path(app, params)
        } else if app.ui.tool_options.shape.mode == "shape" && live_heads.is_some() {
            app.run("shape.edit", params).map(|_| ())
        } else {
            Ok(())
        };
        match result {
            Ok(()) => app.ui.tool_options.shape.arrowheads = heads,
            Err(e) => {
                app.ui.status = e;
                app.ui.status_error = true;
            }
        }
    }
}

#[cfg(test)]
#[path = "shape_modes_tests.rs"]
mod tests;
