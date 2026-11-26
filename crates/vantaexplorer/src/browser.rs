//! Per-pane chrome and file list: tab strip, nav toolbar with clickable breadcrumb,
//! virtualized entry list with sortable columns, collapsible per-pane preview.

use crate::config::Config;
use crate::icons::IconCache;
use crate::model::{format_mtime, human_size, ListingState, SortKey};
use crate::settings::Settings;
use crate::glyph::{self, Glyph};
use crate::panes::{Pane, SplitDir, Workspace};
use std::path::PathBuf;

/// Header row height; independent of zoom so the column controls stay a stable target.
const HEADER_H: f32 = 24.0;

/// Requests the UI cannot execute itself (they mutate the filesystem or app state).
pub enum Action {
    Submit(crate::fileops::Request),
    SetClipboard { paths: Vec<std::path::PathBuf>, cut: bool },
    OpenDropMenu { sources: Vec<std::path::PathBuf>, dest: std::path::PathBuf },
    Paste { dest: std::path::PathBuf },
    Drop { sources: Vec<std::path::PathBuf>, dest: std::path::PathBuf, copy: bool },
    OpenWith(Vec<PathBuf>),
    /// The Properties window for these (Alt+Enter).
    Properties(Vec<PathBuf>),
    /// Measure these directories as one total; replaces any walk in flight.
    CalculateDirSize(Vec<PathBuf>),
    /// A synchronous rename already happened; record it so Ctrl+Z can reverse it.
    RecordRename { from: PathBuf, to: PathBuf },
    /// A file or folder was just created in place (New File / New Folder); undo trashes it.
    RecordCreated(PathBuf),
    /// Start the configured terminal in this folder.
    OpenTerminal(PathBuf),
    /// Ask before deleting permanently. Routed through the action queue rather than
    /// submitted straight to the worker because permanent deletion is the ONE operation
    /// undo cannot reverse (see `OpKind::undoable`).
    ConfirmDelete(Vec<PathBuf>),
    /// Say something to the user in the status bar.
    Toast(String),
    /// Show or hide the preview panel (Space).
    TogglePreview,
    /// Put these trash-view entries back where they came from.
    Restore(Vec<PathBuf>),
    EmptyTrash,
    /// These files were opened (double-click, Enter, the menu's Open): handed to the program
    /// that opens their kind, or, in the chooser, taken as the choice.
    Activate(Vec<PathBuf>),
    /// Put this folder in sidebar section `index` (see `sidebar::Sidebar::targets`).
    AddToSidebar { index: usize, path: PathBuf },
}
#[derive(Clone, Default)]
pub struct DragPayload { 
    pub source_paths: Vec<std::path::PathBuf>,
    pub secondary: bool,
}

/// Whether a drag (between panes, or from another window) is in progress.
fn dragging(ui: &egui::Ui) -> bool {
    ui.ctx().memory(|m| m.data.get_temp::<DragPayload>(egui::Id::new("dnd")).is_some())
}

/// The folder a drop would land in right now, for the note that follows the pointer. Set by
/// whatever is under the pointer this frame; read and cleared by the overlay.
/// Set by main.rs for one Ctrl+Alt+C (copy path), read by the shortcuts below.
pub const COPY_PATH_KEY: &str = "vx-copy-path-key";
/// Set by main.rs for one press of the Menu key, which egui has no key for.
pub const MENU_KEY: &str = "vx-menu-key";
/// A menu opened from the keyboard: which, where, and where in it the keys are.
const KEY_MENU: &str = "vx-key-menu";
/// The menu items that open a submenu (`submenu`), so Right knows which ones do.
const SUBMENUS: &str = "vx-submenus";

#[derive(Clone)]
struct KeyMenu {
    popup: egui::Id,
    at: egui::Pos2,
    /// The menu's own layer, known once it has been drawn.
    layer: Option<egui::LayerId>,
    /// The item the keys are on in each open level: the menu, then its open submenus.
    path: Vec<usize>,
    /// A submenu was just opened from the keys: step into it once it shows.
    opening: bool,
    /// The keys have taken it over. One opened by a right-click is the pointer's until an
    /// arrow, Home or End is pressed; that press marks its first item.
    woke: bool,
}

/// The places held under each level of a menu the keys are steering, for the marking that
/// glides there (`menu_level`).
const MENU_SLOTS: &str = "vx-menu-slots";

/// The menu state behind each menu level's layer (`menu_level`).
const MENU_ROOTS: &str = "vx-menu-roots";

/// Closes the submenu open from the menu level on `layer`.
fn close_submenu_of(ctx: &egui::Context, layer: egui::LayerId) {
    let root = ctx.data(|d| d.get_temp::<Vec<(egui::LayerId, egui::Id)>>(egui::Id::new(MENU_ROOTS))).and_then(|r| r.iter().find(|(l, _)| *l == layer).map(|(_, id)| *id));
    if let Some(root) = root {
        egui::containers::menu::MenuState::from_id(ctx, root, |s| s.open_item = None);
    }
}

/// A level's layer, the place held in it, and whether this frame is egui's sizing pass for
/// it: laid out unseen at full width, its items as wide as the screen.
type MenuSlot = (egui::LayerId, egui::layers::ShapeIdx, bool);

/// A level of a menu, before its items: while the keys steer it, egui's own fill on the item
/// with the focus is turned off and a place held under the items for the marking that glides
/// from item to item instead (`context_popup`).
fn menu_level(ui: &mut egui::Ui) {
    let ctx = ui.ctx().clone();
    // Which menu state this level is, for the keys to close the submenu open from it.
    let root = egui::containers::menu::find_menu_root(ui).id;
    let layer = ui.layer_id();
    ctx.data_mut(|d| {
        let roots = d.get_temp_mut_or_default::<Vec<(egui::LayerId, egui::Id)>>(egui::Id::new(MENU_ROOTS));
        roots.retain(|(l, _)| *l != layer);
        roots.push((layer, root));
        if roots.len() > 32 {
            roots.remove(0);
        }
    });
    let Some(slots) = ctx.data(|d| d.get_temp::<Vec<MenuSlot>>(egui::Id::new(MENU_SLOTS))) else { return };
    let active = &mut ui.visuals_mut().widgets.active;
    active.weak_bg_fill = egui::Color32::TRANSPARENT;
    active.bg_fill = egui::Color32::TRANSPARENT;
    let slot = ui.painter().add(egui::Shape::Noop);
    let mut slots = slots;
    slots.push((ui.layer_id(), slot, ui.is_sizing_pass()));
    ctx.data_mut(|d| d.insert_temp(egui::Id::new(MENU_SLOTS), slots));
}

/// `ui.menu_button` in a menu, noted as a submenu for the keys.
fn submenu<R>(ui: &mut egui::Ui, label: &str, add: impl FnOnce(&mut egui::Ui) -> R) -> egui::InnerResponse<Option<R>> {
    let r = ui.menu_button(label, |ui| {
        menu_level(ui);
        add(ui)
    });
    let id = r.response.id;
    ui.ctx().data_mut(|d| {
        let ids = d.get_temp_mut_or_default::<Vec<egui::Id>>(egui::Id::new(SUBMENUS));
        if !ids.contains(&id) {
            ids.push(id);
            if ids.len() > 256 {
                ids.remove(0);
            }
        }
    });
    r
}

/// The items of a menu layer that can be clicked, top to bottom, as drawn last frame.
fn menu_items(ctx: &egui::Context, layer: egui::LayerId) -> Vec<(egui::Id, egui::Rect)> {
    let mut items: Vec<(egui::Id, egui::Rect)> = ctx.viewport(|v| {
        v.prev_pass.widgets.get_layer(layer).filter(|w| w.enabled && w.sense.senses_click()).map(|w| (w.id, w.rect)).collect()
    });
    // The menu's own area senses clicks too, over all of it; it is no item, and as the topmost
    // it took the first press of Down.
    // So does a widget over the whole of it, the only one there in the frame it first shows.
    let all = items.clone();
    let area = ctx.memory(|m| m.area_rect(layer.id));
    items.retain(|(id, r)| {
        *id != layer.id && !all.iter().any(|(o, q)| o != id && r.contains_rect(*q)) && area.map(|a| a.shrink(2.0)).filter(|a| a.is_positive()).is_none_or(|a| !r.contains_rect(a))
    });
    items.sort_by(|a, b| a.1.min.y.total_cmp(&b.1.min.y));
    items.dedup_by_key(|i| i.0);
    items
}

/// The layer of the submenu `item` opens, while it is open.
fn submenu_layer(ctx: &egui::Context, item: egui::Id) -> Option<egui::LayerId> {
    let id = item.with("submenu");
    // A closed one keeps its layer, with nothing on it.
    ctx.viewport(|v| v.prev_pass.widgets.layer_ids().find(|l| l.id == id && v.prev_pass.widgets.get_layer(*l).next().is_some()))
}

/// Presses Enter for the widget with the focus: how a submenu opens or closes from the keys.
fn press_enter(ctx: &egui::Context) {
    ctx.input_mut(|i| i.events.push(egui::Event::Key { key: egui::Key::Enter, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::NONE }));
}

/// The keys in a menu opened from the keyboard, before it is drawn: Up and Down (Home, End)
/// through its items and never out of it, Right into a submenu, Left out of one. The item
/// they are on has the focus, so Enter picks it the way egui clicks a focused widget, and a
/// ring. egui's own arrow focus is turned off here: it goes to whatever lies nearest that
/// way, outside the menu as readily as in it.
fn steer_menu(ctx: &egui::Context, root: egui::LayerId, k: &mut KeyMenu) {
    let (mut up, mut down, mut home, mut end, mut right, left, enter) = ctx.input(|i| {
        let plain = i.modifiers.is_none();
        (
            plain && i.key_pressed(egui::Key::ArrowUp),
            plain && i.key_pressed(egui::Key::ArrowDown),
            plain && i.key_pressed(egui::Key::Home),
            plain && i.key_pressed(egui::Key::End),
            plain && i.key_pressed(egui::Key::ArrowRight),
            plain && i.key_pressed(egui::Key::ArrowLeft),
            i.key_pressed(egui::Key::Enter) || i.key_pressed(egui::Key::Space),
        )
    });
    ctx.memory_mut(|m| m.move_focus(egui::FocusDirection::None));
    if up || down || home || end || right || left {
        // The keys have the menu: the pointer resting on it is forgotten until it moves.
        // Where it rests, egui closes a submenu the keys opened, the pointer being over
        // another item, and opens the one under it.
        ctx.input_mut(|i| i.pointer = Default::default());
    }
    if !k.woke {
        if !(up || down || home || end || right) {
            return;
        }
        // Taken over: the first item marked, and this press spent on that.
        k.woke = true;
        k.path = vec![0];
        (up, down, home, end, right) = (false, false, false, false, false);
    }
    // The open levels, as far as the path goes and they are still open.
    let mut levels = vec![(root, menu_items(ctx, root))];
    for depth in 0..k.path.len().saturating_sub(1) {
        let Some(&(item, _)) = levels[depth].1.get(k.path[depth]) else { break };
        match submenu_layer(ctx, item) {
            Some(l) => levels.push((l, menu_items(ctx, l))),
            None => break,
        }
    }
    k.path.truncate(levels.len());
    if k.path.is_empty() {
        k.path.push(0);
    }
    let submenus = ctx.data(|d| d.get_temp::<Vec<egui::Id>>(egui::Id::new(SUBMENUS))).unwrap_or_default();
    let current = |levels: &[(egui::LayerId, Vec<(egui::Id, egui::Rect)>)], path: &[usize]| {
        let d = path.len() - 1;
        levels.get(d).and_then(|(l, items)| items.get(path[d]).map(|&(id, r)| (*l, id, r)))
    };
    // A submenu opened from the keys: into it, once it is there.
    if k.opening {
        if let Some((_, item, _)) = current(&levels, &k.path) {
            if let Some(l) = submenu_layer(ctx, item) {
                let items = menu_items(ctx, l);
                if !items.is_empty() {
                    levels.push((l, items));
                    k.path.push(0);
                    k.opening = false;
                }
            } else if !submenus.contains(&item) {
                k.opening = false;
            }
        }
    }
    let d = k.path.len() - 1;
    let n = levels.get(d).map_or(0, |l| l.1.len());
    if n == 0 {
        return;
    }
    let c = k.path[d].min(n - 1);
    k.path[d] = if down {
        (c + 1).min(n - 1)
    } else if up {
        c.saturating_sub(1)
    } else if home {
        0
    } else if end {
        n - 1
    } else {
        c
    };
    let Some((_, item, _)) = current(&levels, &k.path) else { return };
    if left && d > 0 {
        // Out: the submenu closed, back on its own item. Closed directly: pressing Enter on
        // the item, as opening does, came before the focus had moved there, and went to the
        // submenu's item instead.
        k.path.pop();
        if let Some((parent_layer, parent, _)) = current(&levels, &k.path) {
            close_submenu_of(ctx, parent_layer);
            ctx.memory_mut(|m| m.request_focus(parent));
        }
        return;
    }
    if submenus.contains(&item) && (right || enter) {
        match submenu_layer(ctx, item) {
            // Already open (the pointer opened it): just step in.
            Some(l) if right => {
                levels.push((l, menu_items(ctx, l)));
                k.path.push(0);
            }
            Some(_) => {}
            None => {
                if right {
                    press_enter(ctx);
                }
                k.opening = true;
            }
        }
    }
    if let Some((_, id, _)) = current(&levels, &k.path) {
        ctx.memory_mut(|m| m.request_focus(id));
    }
}

/// F10 or the Menu key: the menu a right-click would open, where the keyboard is. With
/// something marked, that entry's (its row opens it as it is drawn, `menu_for`); with nothing
/// marked, the folder's. Returns whether the folder's is to open.
fn menu_key(ui: &egui::Ui, tab: &mut crate::panes::Tab, visible: &[usize], view: &View) -> bool {
    let menu = ui.ctx().data_mut(|d| d.remove_temp::<bool>(egui::Id::new(MENU_KEY))).unwrap_or(false);
    let f10 = ui.input(|i| i.key_pressed(egui::Key::F10));
    if !(menu || f10) || ui.ctx().text_edit_focused() || view.modal_open {
        return false;
    }
    match keyboard_position(tab, visible).filter(|_| !tab.selection.is_empty()) {
        Some(vi) => {
            tab.menu_for = visible.get(vi).and_then(|&ei| tab.listing.entries.get(ei)).map(|e| e.name.clone());
            tab.scroll_to = Some(vi);
            false
        }
        None => true,
    }
}

/// What a right-click does to a context menu, as `Popup::context_menu` decides it.
pub(crate) fn right_click_opens(resp: &egui::Response) -> Option<egui::SetOpenCommand> {
    if resp.secondary_clicked() {
        Some(egui::SetOpenCommand::Bool(true))
    } else if resp.clicked() {
        Some(egui::SetOpenCommand::Bool(false))
    } else {
        None
    }
}

/// A context menu on `resp`: opened by `open` at the pointer, or from the keyboard at `key_at`.
/// One opened from the keyboard stays where it opened and is steered by the keys
/// (`steer_menu`).
pub(crate) fn context_popup(resp: &egui::Response, open: Option<egui::SetOpenCommand>, key_at: Option<egui::Pos2>, content: impl FnOnce(&mut egui::Ui)) {
    let ctx = resp.ctx.clone();
    let key = egui::Id::new(KEY_MENU);
    let id = egui::Popup::default_response_id(resp);
    let mut open = open;
    if let Some(at) = key_at {
        ctx.data_mut(|d| d.insert_temp(key, KeyMenu { popup: id, at, layer: None, path: vec![0], opening: false, woke: true }));
        open = Some(egui::SetOpenCommand::Bool(true));
    } else if resp.secondary_clicked() {
        // Opened at the pointer, for the pointer, until the keys take it over.
        if let Some(at) = resp.interact_pointer_pos() {
            ctx.data_mut(|d| d.insert_temp(key, KeyMenu { popup: id, at, layer: None, path: vec![0], opening: false, woke: false }));
        }
    }
    let mut from_key = ctx.data(|d| d.get_temp::<KeyMenu>(key)).filter(|k| k.popup == id);
    if let Some(k) = from_key.as_mut() {
        if let Some(root) = k.layer {
            steer_menu(&ctx, root, k);
        }
    }
    let mut popup = egui::Popup::context_menu(resp).open_memory(open);
    if let Some(k) = &from_key {
        popup = popup.at_position(k.at);
    }
    // Steered by the keys: each level holds a place for the marking (`menu_level`).
    let slots_id = egui::Id::new(MENU_SLOTS);
    let steered = from_key.as_ref().is_some_and(|k| k.woke);
    if steered {
        ctx.data_mut(|d| d.insert_temp(slots_id, Vec::<(egui::LayerId, egui::layers::ShapeIdx)>::new()));
    }
    // egui's fill for the item with the focus, before `menu_level` turns it off.
    let mark = ctx.global_style().visuals.widgets.active;
    let shown = popup.show(|ui| {
        menu_level(ui);
        content(ui)
    });
    let slots = ctx.data_mut(|d| d.remove_temp::<Vec<MenuSlot>>(slots_id)).unwrap_or_default();
    match (from_key, shown) {
        (Some(mut k), Some(inner)) => {
            k.layer = Some(inner.response.layer_id);
            // The marking on the item the keys are on: filled under it, ringed over it, gliding
            // from the item it was on. Into or out of a submenu it appears in place, each level
            // being its own layer.
            let focused = ctx.memory(|m| m.focused()).filter(|_| k.woke);
            // Where the item stands this frame. Not while its level is being sized: drawn then,
            // the marking was as wide as the screen for a frame and glided in to the menu's width.
            let ring = focused.and_then(|f| ctx.viewport(|v| v.this_pass.widgets.get(f).map(|w| (w.layer_id, w.rect))));
            let level = ring.and_then(|(layer, _)| slots.iter().find(|s| s.0 == layer).copied());
            let ring = ring.filter(|_| level.is_none_or(|s| !s.2));
            // Only on an item: until the items are known the focus can be on the menu's own
            // area, and the marking covered all of it for a frame before gliding in to the item.
            let ring = ring.filter(|(layer, _)| focused.is_some_and(|f| menu_items(&ctx, *layer).iter().any(|(id, _)| *id == f)));
            if let Some((layer, rect)) = ring {
                let at = glide_rect(&ctx, egui::Id::new(("menu-glide", layer.id)), rect, animations(&ctx));
                let painter = ctx.layer_painter(layer);
                if let Some((_, slot, _)) = level {
                    painter.set(slot, egui::Shape::rect_filled(at.expand(mark.expansion), mark.corner_radius, mark.weak_bg_fill));
                }
                let color = ctx.global_style().visuals.selection.stroke.color;
                painter.rect_stroke(at, 3.0, egui::Stroke::new(1.5, color), egui::StrokeKind::Inside);
            }
            ctx.data_mut(|d| d.insert_temp(key, k));
        }
        (Some(_), None) => ctx.data_mut(|d| d.remove::<KeyMenu>(key)),
        _ => {}
    }
}
/// How far the keys scroll the preview this frame, taken by whichever scroll area shows it
/// (`take_preview_scroll`).
const PREVIEW_SCROLL: &str = "vx-preview-scroll";

#[derive(Clone, Copy)]
enum PreviewScroll {
    Lines(f32),
    Pages(f32),
    Top,
    Bottom,
}

/// Called inside each of the preview's scroll areas: scrolls it as the keys asked.
pub fn take_preview_scroll(ui: &egui::Ui) {
    let key = egui::Id::new(PREVIEW_SCROLL);
    let Some(s) = ui.ctx().data(|d| d.get_temp::<PreviewScroll>(key)) else { return };
    ui.ctx().data_mut(|d| d.remove::<PreviewScroll>(key));
    let page = ui.clip_rect().height() * 0.9;
    let down = match s {
        PreviewScroll::Lines(n) => n * 40.0,
        PreviewScroll::Pages(n) => n * page,
        PreviewScroll::Top => -1.0e7,
        PreviewScroll::Bottom => 1.0e7,
    };
    // A positive delta moves the content down, which is scrolling up.
    ui.scroll_with_delta(egui::vec2(0.0, -down));
}

/// The keys while the preview has them: the arrows, Page Up and Down, Home and End scroll it;
/// with the pages of a PDF shown one at a time, Page Up and Down turn them, as Ctrl+Page Up
/// and Down do for every document with pages or sheets. Returns the page step, if any.
fn preview_keys(ui: &egui::Ui, one_at_a_time: bool) -> i64 {
    let (up, down, pg_up, pg_down, home, end, ctrl) = ui.input(|i| {
        let plain = !i.modifiers.alt && !i.modifiers.shift;
        (
            plain && !i.modifiers.ctrl && i.key_pressed(egui::Key::ArrowUp),
            plain && !i.modifiers.ctrl && i.key_pressed(egui::Key::ArrowDown),
            plain && i.key_pressed(egui::Key::PageUp),
            plain && i.key_pressed(egui::Key::PageDown),
            plain && !i.modifiers.ctrl && i.key_pressed(egui::Key::Home),
            plain && !i.modifiers.ctrl && i.key_pressed(egui::Key::End),
            i.modifiers.ctrl,
        )
    });
    let scroll = if up {
        Some(PreviewScroll::Lines(-1.0))
    } else if down {
        Some(PreviewScroll::Lines(1.0))
    } else if pg_up && !ctrl && !one_at_a_time {
        Some(PreviewScroll::Pages(-1.0))
    } else if pg_down && !ctrl && !one_at_a_time {
        Some(PreviewScroll::Pages(1.0))
    } else if home {
        Some(PreviewScroll::Top)
    } else if end {
        Some(PreviewScroll::Bottom)
    } else {
        None
    };
    if let Some(s) = scroll {
        ui.ctx().data_mut(|d| d.insert_temp(egui::Id::new(PREVIEW_SCROLL), s));
    }
    let turn = ctrl || one_at_a_time;
    if turn && pg_up {
        -1
    } else if turn && pg_down {
        1
    } else {
        0
    }
}

/// The Tab pressed this frame while a question is up, taken out before egui saw it (main.rs),
/// with whether Shift came with it. egui would move its own focus with it, out of the dialog
/// and onto whatever is next on screen, and then Enter or Space would click that.
pub const DIALOG_TAB: &str = "vx-dialog-tab";

/// The Tab a question gets this frame: `Some(shift)`.
pub fn dialog_tab(ctx: &egui::Context) -> Option<bool> {
    ctx.data(|d| d.get_temp::<bool>(egui::Id::new(DIALOG_TAB)))
}

/// A question's keys are its own: nothing else keeps egui's focus, which Enter and Space would
/// click as well as the button chosen.
pub fn dialog_owns_keys(ctx: &egui::Context) {
    if let Some(f) = ctx.memory(|m| m.focused()) {
        ctx.memory_mut(|m| m.surrender_focus(f));
    }
}

/// A button in a question's row. Every one is the same size and drawn the same; the marking
/// on the chosen one is the glider's (`choice_marking`), so it glides from button to button. A
/// thicker border on the chosen one made it grow and the row shift each time the choice moved.
pub fn choice_button(ui: &mut egui::Ui, glider: &mut Glider, theme: &Theme, text: egui::RichText, chosen: bool) -> egui::Response {
    let btn = egui::Button::new(text).fill(egui::Color32::TRANSPARENT).stroke(egui::Stroke::new(1.0, theme.text.gamma_multiply(0.3)));
    let r = ui.add(btn);
    glider.button(ui, &r);
    if chosen {
        glider.chosen(r.rect);
    }
    r
}

/// Paints a question's marking where it has got to: filled under the chosen button, its border
/// and a ring around it over it, the ring outside the layout.
pub fn choice_marking(ui: &egui::Ui, glider: Glider, id: &str, theme: &Theme, enabled: bool) {
    let radius = ui.visuals().widgets.inactive.corner_radius.nw as f32;
    if let Some(r) = glider.finish(ui, egui::Id::new(("choice-glide", id)), enabled, false, radius, theme.accent.gamma_multiply(0.35)) {
        let accent = egui::Stroke::new(1.0, theme.accent);
        ui.painter().rect_stroke(r, radius, accent, egui::StrokeKind::Inside);
        ui.painter().rect_stroke(r.expand(1.0), 4.0, accent, egui::StrokeKind::Outside);
    }
}

pub const DROP_TARGET: &str = "dnd-target";

fn mark_drop_target(ui: &egui::Ui, dest: &std::path::Path) {
    ui.ctx().data_mut(|d| d.insert_temp(egui::Id::new(DROP_TARGET), dest.to_path_buf()));
}

/// The drag ends here: into `dest`, or, from the right button, a menu asking Copy or Move.
fn drop_into(ui: &egui::Ui, view: &View, dest: &std::path::Path) {
    if let Some(payload) = ui.ctx().memory_mut(|m| m.data.remove_temp::<DragPayload>(egui::Id::new("dnd"))) {
        if payload.secondary {
            view.actions.borrow_mut().push(Action::OpenDropMenu { sources: payload.source_paths, dest: dest.to_path_buf() });
        } else {
            let copy = ui.input(|i| i.modifiers.ctrl);
            view.actions.borrow_mut().push(Action::Drop { sources: payload.source_paths, dest: dest.to_path_buf(), copy });
        }
    }
}

/// Read-only presentation context handed down the pane tree.
pub struct View<'a> {
    /// Ctrl+Right from the rightmost pane put the keys in the preview (`preview_keys`).
    pub preview_focused: bool,
    pub theme: &'a Theme,
    pub settings: &'a Settings,
    pub preview: &'a crate::preview::Preview,
    /// Where the HDR layer should draw the preview image, in POINTS. Written by the pane that
    /// owns the preview; the caller scales it to physical pixels and hands it to the renderer.
    pub hdr_rect: &'a std::cell::Cell<Option<egui::Rect>>,
    /// Path the active pane wants previewed this frame.
    pub want_preview: &'a std::cell::Cell<Option<(std::path::PathBuf, u32)>>,
    pub thumbs: &'a std::cell::RefCell<crate::thumbs::Thumbs>,
    pub actions: &'a std::cell::RefCell<Vec<Action>>,
    pub clipboard_filled: bool,
    /// Files cut to the clipboard, drawn faded until they are pasted.
    pub cut: &'a std::collections::HashSet<std::path::PathBuf>,
    pub dir_size: &'a crate::dir_size::DirSize,
    /// Ctrl+V was pressed, detected on the raw winit event.
    ///
    /// egui-winit only emits `Event::Paste` when it can READ the system clipboard, so a
    /// failed read means the event never arrives. Pasting FILES uses our own clipboard and
    /// must not depend on that round trip at all.
    pub paste_key: bool,
    /// A modal dialog owns the keyboard this frame.
    ///
    /// egui's Modal backdrop blocks the POINTER, but the pane shortcuts read raw key state,
    /// so without this an Enter meant for a dialog button would also fire the pane's "open
    /// selection", confirming a delete and opening a file in one keystroke.
    pub modal_open: bool,
    /// Set when the window is choosing a file for another program (see `chooser.rs`): one pane,
    /// no tabs, nothing that moves or deletes, and only the entries the filter lets through.
    pub chooser: Option<&'a crate::chooser::State>,
    /// The sidebar sections a folder can be added to, for "Add to sidebar".
    pub sidebar_targets: &'a [crate::sidebar::Target],
    /// The file open for editing in the preview pane, if any.
    pub editor: &'a std::cell::RefCell<Option<crate::editor::Editor>>,
    /// What the editor asked for this frame.
    pub editor_event: &'a std::cell::Cell<Option<crate::editor::Event>>,
    /// Ctrl+E was pressed: edit the selected file.
    pub edit_key: bool,
    /// The folders' views, when they are kept per folder and may be changed here (not in the
    /// chooser): for the Folder view menu.
    pub views: Option<&'a std::cell::RefCell<crate::views::Views>>,
}

/// "Folder view" in a folder's context menu: its view for its subfolders too, for every folder,
/// or forgotten.
fn folder_view_menu(ui: &mut egui::Ui, folder: &std::path::Path, view: &View) {
    let Some(views) = view.views else { return };
    if crate::fileops::in_trash(folder) {
        return;
    }
    submenu(ui, "Folder view", |ui| {
        let mut views = views.borrow_mut();
        let own = views.own(folder);
        let mut subfolders = own.is_some_and(|v| v.subfolders);
        if ui.checkbox(&mut subfolders, "Use this view in subfolders").on_hover_text("Subfolders without a view of their own show this one").changed() {
            views.set_subfolders(folder, subfolders);
            ui.close();
        }
        if ui.button("Use this view as the default").on_hover_text("For every folder without a view of its own").clicked() {
            let zoom = views.effective(folder);
            views.set_default(zoom);
            ui.close();
        }
        if own.is_some() && ui.button("Forget this folder's view").on_hover_text("Show it as its parent or the default says").clicked() {
            views.forget(folder);
            ui.close();
        }
    });
}

/// Whether an entry is listed: not hidden (unless asked for), matching what is typed, and let
/// through by the chooser's filter when there is one. `typed` is already lower case.
pub fn entry_visible(e: &crate::model::Entry, show_hidden: bool, typed: &str, chooser: Option<&crate::chooser::State>) -> bool {
    (show_hidden || !e.hidden) && (typed.is_empty() || e.name.to_lowercase().contains(typed)) && chooser.is_none_or(|c| c.passes(&e.name, e.is_dir))
}

pub struct Theme {
    pub accent: egui::Color32,
    pub text: egui::Color32,
    pub icon_tint: [u8; 3],
    /// The folders in Explorer's own icons.
    pub folder: [u8; 3],
    /// The accent where it is drawn as text or as a thin glyph. On a dark palette the accent
    /// itself; on a light one it is darkened until it reads on the window, because a light
    /// theme's accent (Breeze's blue) is made for fills and frames and is too pale for letters.
    pub accent_text: egui::Color32,
    /// Failures, and what cannot be undone. On a dark palette the light red it has always been;
    /// on a light one the palette's error color, darkened until it reads when it does not.
    pub error: egui::Color32,
}

impl Theme {
    pub fn from_config(cfg: &Config) -> Self {
        let text = parse32(&cfg.text).unwrap_or(egui::Color32::WHITE);
        let accent = parse32(&cfg.accent).unwrap_or(text);
        let folder = parse32(&cfg.folder).unwrap_or(accent);
        let light = vanta_ui::theme::is_light(text);
        let (accent_text, error) = if light {
            (vanta_ui::theme::legible(accent, true), vanta_ui::theme::legible(vanta_ui::theme::color32(cfg.colors.error), true))
        } else {
            (accent, egui::Color32::LIGHT_RED)
        };
        Self { accent, text, icon_tint: [text.r(), text.g(), text.b()], folder: [folder.r(), folder.g(), folder.b()], accent_text, error }
    }

    /// A dark theme: light text on the window.
    pub fn is_dark(&self) -> bool {
        !vanta_ui::theme::is_light(self.text)
    }
}

fn parse32(hex: &str) -> Option<egui::Color32> {
    let h = hex.trim().strip_prefix('#')?;
    let (r, g, b) = match h.len() {
        6 => (0..3)
            .map(|i| u8::from_str_radix(&h[i * 2..i * 2 + 2], 16).unwrap_or(0))
            .collect::<Vec<_>>()
            .into_iter()
            .collect_tuple()?,
        8 => (1..4)
            .map(|i| u8::from_str_radix(&h[i * 2..i * 2 + 2], 16).unwrap_or(0))
            .collect::<Vec<_>>()
            .into_iter()
            .collect_tuple()?,
        _ => return None,
    };
    Some(egui::Color32::from_rgb(r, g, b))
}

trait CollectTuple<T> {
    fn collect_tuple(self) -> Option<(T, T, T)>;
}

impl<I: Iterator<Item = u8>> CollectTuple<u8> for I {
    fn collect_tuple(mut self) -> Option<(u8, u8, u8)> {
        Some((self.next()?, self.next()?, self.next()?))
    }
}

/// Render the whole split tree into `rect`. Returns true when app state changed in a way
/// that should persist (navigation, split, close).
pub fn show_workspace(
    ui: &mut egui::Ui,
    ws: &mut Workspace,
    icons: &mut IconCache,
    view: &View,
    rect: egui::Rect,
) {
    // Recursive rendering wants disjoint borrows; use indices and a worklist instead.
    let mut jobs = vec![(clone_layout(&ws.tree), rect)];
    let mut ratio_updates: Vec<(Vec<bool>, f32)> = Vec::new();
    let mut pane_rects: Vec<(usize, egui::Rect)> = Vec::new();

    while let Some((node, r)) = jobs.pop() {
        match node {
            LayoutNode::Leaf(idx) => pane_rects.push((idx, r)),
            LayoutNode::Split { dir, ratio, path, a, b } => {
                const GAP: f32 = 4.0;
                let (ra, rb, splitter) = match dir {
                    SplitDir::Horizontal => {
                        let w = r.width() * ratio;
                        let ra = egui::Rect::from_min_size(r.min, egui::vec2(w - GAP / 2.0, r.height()));
                        let rb = egui::Rect::from_min_max(
                            egui::pos2(r.min.x + w + GAP / 2.0, r.min.y),
                            r.max,
                        );
                        let sp = egui::Rect::from_min_max(
                            egui::pos2(r.min.x + w - GAP / 2.0, r.min.y),
                            egui::pos2(r.min.x + w + GAP / 2.0, r.max.y),
                        );
                        (ra, rb, sp)
                    }
                    SplitDir::Vertical => {
                        let h = r.height() * ratio;
                        let ra = egui::Rect::from_min_size(r.min, egui::vec2(r.width(), h - GAP / 2.0));
                        let rb = egui::Rect::from_min_max(
                            egui::pos2(r.min.x, r.min.y + h + GAP / 2.0),
                            r.max,
                        );
                        let sp = egui::Rect::from_min_max(
                            egui::pos2(r.min.x, r.min.y + h - GAP / 2.0),
                            egui::pos2(r.max.x, r.min.y + h + GAP / 2.0),
                        );
                        (ra, rb, sp)
                    }
                };
                let id = ui.id().with(("splitter", &path));
                let resp = ui.interact(splitter, id, egui::Sense::drag());
                if resp.hovered() || resp.dragged() {
                    ui.ctx().set_cursor_icon(match dir {
                        SplitDir::Horizontal => egui::CursorIcon::ResizeHorizontal,
                        SplitDir::Vertical => egui::CursorIcon::ResizeVertical,
                    });
                }
                if resp.dragged() {
                    let delta = resp.drag_delta();
                    let new_ratio = match dir {
                        SplitDir::Horizontal => ratio + delta.x / r.width(),
                        SplitDir::Vertical => ratio + delta.y / r.height(),
                    };
                    ratio_updates.push((path.clone(), new_ratio.clamp(0.15, 0.85)));
                }
                let mut pa = path.clone();
                pa.push(false);
                let mut pb = path;
                pb.push(true);
                jobs.push((relabel(*a, pa), ra));
                jobs.push((relabel(*b, pb), rb));
            }
        }
    }

    for (path, ratio) in ratio_updates {
        set_ratio(&mut ws.tree, &path, ratio);
    }

    // Draw panes; collect actions to apply after (split/close mutate the tree).
    let mut actions: Vec<PaneAction> = Vec::new();
    let pane_count = ws.panes.len();
    let active = ws.active;
    // Record the geometry for visual-order pane cycling (Ctrl+Tab). The handler runs at
    // the top of the next frame, so last frame's layout is exactly what the user sees.
    ws.visual_rects = pane_rects
        .iter()
        .map(|(i, r)| (*i, r.min.x, r.min.y))
        .collect();
    ws.pane_boxes = pane_rects.clone();

    // The sidebar's button is in the pane that stands against it, the top left one. The chooser
    // has no sidebar toggle (F9 does nothing there either).
    let beside_sidebar = ws.visual_order().first().copied().filter(|_| view.chooser.is_none());
    let sidebar_hidden = ws.sidebar_hidden;

    for (idx, r) in pane_rects {
        let is_active = idx == active;
        let sidebar = (beside_sidebar == Some(idx)).then_some(sidebar_hidden);
        let pane = &mut ws.panes[idx];
        // Its own id per pane: without it both panes' widgets got the same ids, and a click on
        // one pane's button (the path pencil, back, forward) counted in the other as well.
        let mut child = ui.new_child(egui::UiBuilder::new().id_salt(("pane", idx)).max_rect(r).layout(egui::Layout::top_down(egui::Align::Min)));
        // `max_rect` bounds LAYOUT only, painting still inherits the parent's clip rect, so
        // anything drawn with an explicit painter position (headers, row columns, breadcrumbs)
        // escapes the pane. Clip so a pane can never smear over its neighbour or under the
        // preview panel, whatever the split tree hands us.
        child.shrink_clip_rect(r);
        let act = show_pane(&mut child, pane, idx, is_active, pane_count, sidebar, icons, view);
        if let Some(a) = act {
            actions.push(a);
        }
        // Click anywhere in the pane focuses it. Passive pointer check, an interact()
        // overlay here would sit on top of every widget in the pane and eat their clicks.
        let clicked_inside = ui.input(|i| i.pointer.primary_pressed())
            && ui.rect_contains_pointer(r);
        if clicked_inside {
            ws.active = idx;
        }
    }

    for act in actions {
        match act {
            PaneAction::Focus(i) => ws.active = i,
            PaneAction::ToggleSidebar => ws.sidebar_hidden = !ws.sidebar_hidden,
            PaneAction::Split(i, dir) => ws.split(i, dir),
            PaneAction::Close(i) => ws.close(i),
        }
    }
}

enum PaneAction {
    Focus(usize),
    ToggleSidebar,
    Split(usize, SplitDir),
    Close(usize),
}

// Lightweight copy of the tree for layout traversal (borrows must stay disjoint).
enum LayoutNode {
    Leaf(usize),
    Split { dir: SplitDir, ratio: f32, path: Vec<bool>, a: Box<LayoutNode>, b: Box<LayoutNode> },
}

fn clone_layout(n: &crate::panes::Node) -> LayoutNode {
    match n {
        crate::panes::Node::Leaf(i) => LayoutNode::Leaf(*i),
        crate::panes::Node::Split { dir, ratio, a, b } => LayoutNode::Split {
            dir: *dir,
            ratio: *ratio,
            path: Vec::new(),
            a: Box::new(clone_layout(a)),
            b: Box::new(clone_layout(b)),
        },
    }
}

fn relabel(n: LayoutNode, path: Vec<bool>) -> LayoutNode {
    match n {
        LayoutNode::Leaf(i) => LayoutNode::Leaf(i),
        LayoutNode::Split { dir, ratio, a, b, .. } => LayoutNode::Split { dir, ratio, path, a, b },
    }
}

fn set_ratio(node: &mut crate::panes::Node, path: &[bool], ratio: f32) {
    match node {
        crate::panes::Node::Leaf(_) => {}
        crate::panes::Node::Split { ratio: r, a, b, .. } => {
            if path.is_empty() {
                *r = ratio;
            } else if path[0] {
                set_ratio(b, &path[1..], ratio);
            } else {
                set_ratio(a, &path[1..], ratio);
            }
        }
    }
}

#[allow(clippy::too_many_arguments)] // a context struct here would just move the noise
fn show_pane(
    ui: &mut egui::Ui,
    pane: &mut Pane,
    idx: usize,
    is_active: bool,
    pane_count: usize,
    // Whether the sidebar is hidden, when this pane carries the button that shows and hides it.
    sidebar: Option<bool>,
    icons: &mut IconCache,
    view: &View,
) -> Option<PaneAction> {
    let theme = view.theme;
    let mut action = None;

    // Mouse back/forward buttons navigate the hovered pane's history, browser-style.
    if ui.rect_contains_pointer(ui.max_rect()) {
        let (back, fwd) = ui.input(|i| {
            (
                i.pointer.button_released(egui::PointerButton::Extra1),
                i.pointer.button_released(egui::PointerButton::Extra2),
            )
        });
        if back {
            pane.tab().back();
            action = Some(PaneAction::Focus(idx));
        } else if fwd {
            pane.tab().forward();
            action = Some(PaneAction::Focus(idx));
        }
    }

    // --- tab strip ---
    // Not in the chooser: one folder is being looked for, in one pane.
    if view.chooser.is_none() {
    ui.horizontal(|ui| {
        let mut close_tab: Option<usize> = None;
        let mut select_tab: Option<usize> = None;
        for (i, tab) in pane.tabs.iter().enumerate() {
            let selected = i == pane.active_tab;
            let resp = ui.selectable_label(selected, tab.title());
            if resp.clicked() {
                select_tab = Some(i);
            }
            if resp.middle_clicked() && pane.tabs.len() > 1 {
                close_tab = Some(i);
            }
        }
        if ui.button("+").on_hover_text("New tab").clicked() {
            let path = pane.tab_ref().path().to_path_buf();
            pane.tabs.push(crate::panes::Tab::new(path));
            pane.active_tab = pane.tabs.len() - 1;
            action = Some(PaneAction::Focus(idx));
        }
        if let Some(i) = close_tab {
            pane.close_tab(i);
        }
        if let Some(i) = select_tab {
            pane.active_tab = i;
            action = Some(PaneAction::Focus(idx));
        }

        if is_active {
            let (new_tab, close_tab, next_tab, prev_tab, reopen_tab) = ui.input(|i| (
                i.modifiers.ctrl && !i.modifiers.shift && i.key_pressed(egui::Key::T),
                i.modifiers.ctrl && !i.modifiers.shift && i.key_pressed(egui::Key::W),
                (i.modifiers.ctrl && i.key_pressed(egui::Key::PageDown)) || (i.modifiers.ctrl && !i.modifiers.shift && i.key_pressed(egui::Key::Tab)),
                (i.modifiers.ctrl && i.key_pressed(egui::Key::PageUp)) || (i.modifiers.ctrl && i.modifiers.shift && i.key_pressed(egui::Key::Tab)),
                i.modifiers.ctrl && i.modifiers.shift && i.key_pressed(egui::Key::T),
            ));
            if new_tab {
                let path = pane.tab_ref().path().to_path_buf();
                pane.tabs.push(crate::panes::Tab::new(path));
                pane.active_tab = pane.tabs.len() - 1;
            } else if reopen_tab {
                // The last folder closed here, if it still exists.
                while let Some(path) = pane.closed_tabs.pop() {
                    if path.is_dir() {
                        pane.tabs.push(crate::panes::Tab::new(path));
                        pane.active_tab = pane.tabs.len() - 1;
                        break;
                    }
                }
            } else if close_tab {
                if pane.tabs.len() > 1 {
                    let i = pane.active_tab;
                    pane.close_tab(i);
                } else {
                    action = Some(PaneAction::Close(idx));
                }
            } else if next_tab {
                pane.active_tab = (pane.active_tab + 1) % pane.tabs.len();
            } else if prev_tab {
                pane.active_tab = (pane.active_tab + pane.tabs.len() - 1) % pane.tabs.len();
            }
        }

        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let (n, h) = (theme.text.gamma_multiply(0.75), theme.accent_text);
            if pane_count > 1
                && glyph::button(ui, Glyph::Cross, true, "Close pane", n, h)
            {
                action = Some(PaneAction::Close(idx));
            }
            if glyph::button(ui, Glyph::SplitV, true, "Split top/bottom", n, h) {
                action = Some(PaneAction::Split(idx, SplitDir::Vertical));
            }
            if glyph::button(ui, Glyph::SplitH, true, "Split left/right", n, h) {
                action = Some(PaneAction::Split(idx, SplitDir::Horizontal));
            }
        });
    });
    }

    // --- nav toolbar ---
    let mut navigate: Option<PathBuf> = None;
    ui.horizontal(|ui| {
        let (n, h) = (theme.text.gamma_multiply(0.75), theme.accent_text);
        let tab = pane.tab();
        let (can_back, can_fwd, has_parent) =
            (tab.can_back(), tab.can_forward(), tab.path().parent().is_some() && !crate::fileops::in_trash(tab.path()));
        if let Some(hidden) = sidebar {
            let tip = if hidden { "Show the sidebar (F9)" } else { "Hide the sidebar (F9)" };
            if glyph::button(ui, Glyph::Sidebar(!hidden), true, tip, n, h) {
                action = Some(PaneAction::ToggleSidebar);
            }
        }
        if glyph::button(ui, Glyph::ChevronLeft, can_back, "Back", n, h) {
            tab.back();
        }
        if glyph::button(ui, Glyph::ChevronRight, can_fwd, "Forward", n, h) {
            tab.forward();
        }
        if glyph::button(ui, Glyph::Up, has_parent, "Up", n, h) {
            tab.up();
        }
        if glyph::button(ui, Glyph::Reload, true, "Reload", n, h) {
            tab.reload();
        }
        // The path as editable text, always reachable: a long path can fill the whole row and
        // leave no empty space to click. A second click closes it again.
        if glyph::button(ui, Glyph::Edit, true, "Edit path (Ctrl+L)", n, h) {
            let tab = pane.tab();
            tab.path_edit = match tab.path_edit {
                Some(_) => None,
                None => Some(tab.path().to_string_lossy().into_owned()),
            };
        }

        // Right-side toggles first (right_to_left), breadcrumb takes the rest.
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui
                .selectable_label(pane.show_hidden, "Hidden")
                .on_hover_text("Show hidden files")
                .clicked()
            {
                pane.show_hidden = !pane.show_hidden;
            }

            ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                // Whatever does not fit is cut off here rather than drawn under the Hidden
                // button.
                ui.set_clip_rect(ui.clip_rect().intersect(ui.max_rect()));
                let tab = pane.tab();
                if let Some(edit) = &mut tab.path_edit {
                    // Exactly the room there is: wider, the end of a long path hid under the
                    // Hidden button, where the field could not be scrolled to it.
                    let resp = ui.add(
                        egui::TextEdit::singleline(edit).desired_width(ui.available_width()),
                    );
                    // Focus is requested exactly ONCE, when the field opens.
                    //
                    // It used to be re-requested every frame to keep the field open, which
                    // looked harmless and was not: Memory::request_focus replaces the focused
                    // widget wholesale, resetting its event filter to the default, where
                    // arrows, Tab and Escape are all excluded. set_focus_lock_filter only
                    // re-arms the filter for a widget that ALSO had focus last frame, so the
                    // field never got its arrow keys back, the caret could not be moved and
                    // the path could not be selected with the keyboard at all.
                    if !tab.path_edit_focused {
                        tab.path_edit_focused = true;
                        resp.request_focus();
                        // Open with everything selected, the way a browser's address bar
                        // does: ctrl+L then ctrl+C copies the path, and typing replaces it.
                        if let Some(mut st) = egui::TextEdit::load_state(ui.ctx(), resp.id) {
                            let end = egui::text::CCursor::new(edit.chars().count());
                            st.cursor.set_char_range(Some(egui::text::CCursorRange::two(
                                egui::text::CCursor::new(0),
                                end,
                            )));
                            st.store(ui.ctx(), resp.id);
                        }
                    }
                    let confirmed = resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                    let cancelled = ui.input(|i| i.key_pressed(egui::Key::Escape));
                    if confirmed {
                        let p = PathBuf::from(shellexpand_home(edit));
                        if p.is_dir() {
                            navigate = Some(p);
                        }
                        tab.path_edit = None;
                    } else if cancelled || resp.lost_focus() {
                        // Clicking away closes it, as every address bar does.
                        tab.path_edit = None;
                    }
                } else {
                    tab.path_edit_focused = false;
                    // Clickable breadcrumb segments.
                    // The trash is one place, on every drive at once: "Trash", not the home
                    // trash's files/ folder it happens to start from.
                    let in_trash = crate::fileops::in_trash(tab.path());
                    let comps: Vec<PathBuf> = if in_trash {
                        vec![tab.path().to_path_buf()]
                    } else {
                        let mut acc = PathBuf::new();
                        tab.path()
                            .components()
                            .map(|c| {
                                acc.push(c);
                                acc.clone()
                            })
                            .collect()
                    };
                    let labels: Vec<String> = comps
                        .iter()
                        .map(|c| if in_trash { "Trash".to_string() } else {
                            c.file_name()
                                .map(|n| n.to_string_lossy().into_owned())
                                .unwrap_or_else(|| "/".into())
                        })
                        .collect();

                    // Drop LEADING components when the path will not fit, instead of letting
                    // it run on under the Hidden button (nothing clips this row). The tail is
                    // what tells you where you are; "/home/you" does not. Widths are
                    // measured from real glyphs, theme fonts vary too much to guess.
                    let font = egui::TextStyle::Button.resolve(ui.style());
                    let text_w = |t: &str| {
                        ui.ctx()
                            .fonts_mut(|f| {
                                f.layout_no_wrap(t.to_string(), font.clone(), egui::Color32::WHITE)
                            })
                            .size()
                            .x
                    };
                    let pad = ui.spacing().button_padding.x * 2.0;
                    // Every segment but the first also carries a chevron and its spacing.
                    let chevron = 10.0 + ui.spacing().item_spacing.x * 2.0;
                    let avail = ui.available_width();
                    let full: f32 = labels
                        .iter()
                        .enumerate()
                        .map(|(i, l)| text_w(l) + pad + if i > 0 { chevron } else { 0.0 })
                        .sum();

                    let mut first = 0usize;
                    let mut last_label = labels.last().cloned().unwrap_or_default();
                    if full > avail && !labels.is_empty() {
                        // Everything shown now follows the "..." marker, so every kept
                        // segment carries a chevron.
                        let budget = avail - (text_w("…") + pad + chevron);
                        let mut used = 0.0;
                        // The last component is always kept, even if it alone overflows:
                        // a breadcrumb showing nothing would be worse than one that spills.
                        first = labels.len() - 1;
                        for i in (0..labels.len()).rev() {
                            let w = text_w(&labels[i]) + pad + chevron;
                            if used + w > budget {
                                break;
                            }
                            used += w;
                            first = i;
                        }
                        // The last component alone is too long for the room (a narrow pane,
                        // a wide font): shortened to fit, so it never reaches under Hidden.
                        let n = labels.len();
                        let room = if n > 1 { budget - chevron } else { avail } - pad;
                        if first == n - 1 && text_w(&last_label) > room {
                            last_label = crate::text::elide_end(ui, &last_label, room.max(0.0));
                        }
                    }

                    if first > 0 {
                        // Clicking the marker opens the editable field, so the full path is
                        // always one click away.
                        let full_path = tab.path().to_string_lossy().into_owned();
                        if ui
                            .selectable_label(false, "…")
                            .on_hover_text(&full_path)
                            .clicked()
                        {
                            tab.path_edit = Some(full_path);
                        }
                    }

                    for (i, comp) in comps.iter().enumerate().skip(first) {
                        let label = if i + 1 == labels.len() { last_label.clone() } else { labels[i].clone() };
                        if i > 0 {
                            let (r, _) = ui.allocate_exact_size(
                                egui::vec2(10.0, 14.0),
                                egui::Sense::hover(),
                            );
                            glyph::paint(
                                ui.painter(),
                                r,
                                Glyph::ChevronRight,
                                theme.text.gamma_multiply(0.45),
                            );
                        }
                        let seg = ui.selectable_label(false, label);
                        if seg.clicked() {
                            navigate = Some(comp.clone());
                        }
                        // Each folder in the path is a drop target too: the one shown here, or
                        // any above it, also when the pane is full of subfolders.
                        if dragging(ui) && seg.contains_pointer() {
                            ui.painter().rect_filled(seg.rect, 3.0, theme.accent.gamma_multiply(0.5));
                            mark_drop_target(ui, comp);
                            if ui.input(|i| i.pointer.any_released()) {
                                drop_into(ui, view, comp);
                            }
                        }
                    }
                    // Click in the empty rest of the row -> editable path field.
                    let rest = ui.available_rect_before_wrap();
                    if ui
                        .interact(rest, ui.id().with("path-edit-zone"), egui::Sense::click())
                        .clicked()
                    {
                        tab.path_edit = Some(tab.path().to_string_lossy().into_owned());
                    }
                }
            });
        });
    });

    if let Some(p) = navigate {
        pane.tab().navigate(p);
        action = Some(PaneAction::Focus(idx));
    }

    // Active-pane accent line under the toolbar. Under the others a quiet line: the dark gray
    // it has always been on a dark theme, the text color faded on a light one, where that gray
    // would stand out as much as the accent.
    let line = ui.available_rect_before_wrap();
    let line = egui::Rect::from_min_size(line.min, egui::vec2(line.width(), 2.0));
    let quiet = if theme.is_dark() { egui::Color32::from_gray(60) } else { theme.text.gamma_multiply(0.25) };
    ui.painter().rect_filled(line, 0.0, if is_active { theme.accent } else { quiet });
    ui.add_space(4.0);

    // --- content: the list fills the pane; the preview lives at window level ---
    let typeahead_before = pane.tab_ref().typeahead.clone();
    if is_active && !ui.ctx().text_edit_focused() && !view.modal_open {
        // Navigation keys. None of these are intercepted by egui-winit (only Ctrl+C/X/V
        // are), so plain key tests are fine. Alt+arrows are history/parent here and must
        // NOT also move the selection: keyboard_nav ignores arrows while alt is held.
        let (back, fwd, up, reload, edit_path, hidden) = ui.input(|i| {
            let alt = i.modifiers.alt && !i.modifiers.ctrl;
            (
                alt && i.key_pressed(egui::Key::ArrowLeft),
                alt && i.key_pressed(egui::Key::ArrowRight),
                alt && i.key_pressed(egui::Key::ArrowUp),
                i.key_pressed(egui::Key::F5),
                i.modifiers.ctrl && !i.modifiers.alt && i.key_pressed(egui::Key::L),
                i.modifiers.ctrl && !i.modifiers.alt && i.key_pressed(egui::Key::H),
            )
        });
        if hidden {
            pane.show_hidden = !pane.show_hidden;
        }
        let (home, find, zoom_in, zoom_out, zoom_reset, list_view, grid_view) = ui.input(|i| {
            let ctrl = i.modifiers.ctrl && !i.modifiers.alt;
            (
                i.modifiers.alt && !i.modifiers.ctrl && i.key_pressed(egui::Key::Home),
                ctrl && i.key_pressed(egui::Key::F),
                ctrl && (i.key_pressed(egui::Key::Plus) || i.key_pressed(egui::Key::Equals)),
                ctrl && i.key_pressed(egui::Key::Minus),
                ctrl && i.key_pressed(egui::Key::Num0),
                ctrl && i.key_pressed(egui::Key::Num1),
                ctrl && i.key_pressed(egui::Key::Num2),
            )
        });
        if home {
            pane.tab().navigate(crate::panes::home_dir());
        }
        if find {
            // Opens the filter bar searching subfolders, ready for typing.
            let tab = pane.tab();
            tab.search_open = true;
            if !tab.recursive {
                tab.recursive = true;
                if !tab.typeahead.is_empty() {
                    tab.reload();
                }
            }
        }
        {
            use crate::panes::{DEFAULT_ZOOM, GRID_THRESHOLD, MAX_ZOOM, MIN_ZOOM};
            // Keyboard steps of a quarter, the same zoom Ctrl+scroll drives (and, like it,
            // crossing into the grid past the threshold); Ctrl+0 goes back to the default size
            // of whichever view is showing; Ctrl+1 and Ctrl+2 pick the view.
            let grid_default = GRID_THRESHOLD * 2.4;
            if zoom_in {
                pane.zoom = (pane.zoom * 1.25).clamp(MIN_ZOOM, MAX_ZOOM);
            } else if zoom_out {
                pane.zoom = (pane.zoom / 1.25).clamp(MIN_ZOOM, MAX_ZOOM);
            } else if zoom_reset {
                pane.zoom = if pane.is_grid() { grid_default } else { DEFAULT_ZOOM };
            } else if list_view && pane.is_grid() {
                pane.zoom = DEFAULT_ZOOM;
            } else if grid_view && !pane.is_grid() {
                pane.zoom = grid_default;
            }
        }
        {
            let tab = pane.tab();
            if back {
                tab.back();
            } else if fwd {
                tab.forward();
            } else if up {
                tab.up();
            } else if reload {
                tab.reload();
            } else if edit_path {
                tab.path_edit = Some(tab.path().to_string_lossy().into_owned());
            }
        }

        let events = ui.input(|i| i.events.clone());
        let tab = pane.tab();
        for event in events {
            match event {
                // Space with nothing typed shows or hides the preview; once a filter has begun,
                // it is part of the name being looked for.
                egui::Event::Text(s) if s == " " && tab.typeahead.is_empty() && !tab.search_open => {
                    view.actions.borrow_mut().push(Action::TogglePreview);
                }
                egui::Event::Text(s) => {
                    if s.chars().all(|c| !c.is_control()) {
                        tab.typeahead.push_str(&s);
                    }
                }
                egui::Event::Key { key: egui::Key::Escape, pressed: true, .. } => {
                    tab.typeahead.clear();
                    tab.search_open = false;
                }
                // Backspace does double duty: erase from the filter while there is one, and
                // go back a step when there is not, as in Dolphin and Windows Explorer (the
                // parent folder is Left in the list, and Alt+Up). Two arms with a pure guard, so
                // the side effect is never hidden inside a match condition.
                egui::Event::Key { key: egui::Key::Backspace, pressed: true, .. }
                    if tab.typeahead.is_empty() =>
                {
                    tab.back();
                }
                egui::Event::Key { key: egui::Key::Backspace, pressed: true, .. } => {
                    tab.typeahead.pop();
                }
                _ => {}
            }
        }
    }

    // Typing re-runs the walk. The listing worker is cancelled by a failed send when the
    // receiver is dropped, so an abandoned keystroke's search stops instead of running on.
    if pane.tab_ref().recursive && pane.tab_ref().typeahead != typeahead_before {
        pane.tab().reload();
    }

    if !pane.tab_ref().typeahead.is_empty() || pane.tab_ref().search_open {
        let mut toggled: Option<bool> = None;
        ui.horizontal(|ui| {
            let what = if pane.tab_ref().recursive { "Search:" } else { "Filter:" };
            ui.label(egui::RichText::new(what).color(theme.text.gamma_multiply(0.6)));
            if pane.tab_ref().typeahead.is_empty() {
                ui.label(egui::RichText::new("type to search, Esc to close").color(theme.text.gamma_multiply(0.4)));
            }
            ui.label(egui::RichText::new(&pane.tab_ref().typeahead).strong());
            let mut recursive = pane.tab_ref().recursive;
            if ui
                .checkbox(&mut recursive, "Search subfolders")
                .on_hover_text("Results show their path relative to this folder")
                .changed()
            {
                toggled = Some(recursive);
            }
        });
        if let Some(recursive) = toggled {
            let tab = pane.tab();
            tab.recursive = recursive;
            // Switching modes changes what the listing IS, so it has to be refetched either
            // way: on, to walk the tree; off, to get the plain folder back.
            tab.reload();
        }
        ui.add_space(4.0);
    }

    let list_rect = ui.available_rect_before_wrap();

    let mut list_ui = ui.new_child(egui::UiBuilder::new().max_rect(list_rect).layout(egui::Layout::top_down(egui::Align::Min)));
    let focus = show_list(&mut list_ui, pane, idx, is_active, icons, view);
    if focus {
        action = Some(PaneAction::Focus(idx));
    }

    action
}

/// One source of truth for column x-positions, used by header and rows alike so they can
/// never drift apart.
///
/// Columns flow LEFT TO RIGHT: Name has a width, Size starts where Name ends, Modified starts
/// where Size ends and takes the remainder. They used to be right-anchored to the pane edge,
/// so shrinking Name merely opened a gap instead of pulling the other columns leftward.
/// The behaviour wanted here is the file-manager one: the columns follow.
struct Cols {
    name_left: f32,
    name_w: f32,
    size_left: f32,
    size_w: f32,
    modified_left: f32,
    modified_w: f32,
    right: f32,
    /// False when the pane is too narrow to hold the column. Callers must skip painting it
    /// entirely rather than draw it at a degenerate position.
    show_size: bool,
    show_modified: bool,
}

const MIN_NAME_W: f32 = 60.0;
/// How much of a cut file is drawn until it is pasted.
const CUT_ALPHA: f32 = 0.4;
const MIN_SIZE_W: f32 = 40.0;
const MIN_MODIFIED_W: f32 = 40.0;
/// The name keeps at least this much before Modified is shown at all: names cut to a few
/// letters are worth less than the dates.
const NAME_BEFORE_MODIFIED: f32 = 160.0;

/// What the Size and Modified columns need to show their text whole, measured in the font in
/// use. Fixed minimums were made for a narrow font: in a wide one (a monospace one, say) a
/// squeezed pane showed sizes as "…KiB" and dates as "202…".
#[derive(Clone, Copy)]
struct ColNeed {
    size: f32,
    modified: f32,
}

impl ColNeed {
    fn measure(ui: &egui::Ui, date_format: &str) -> Self {
        let font = egui::TextStyle::Body.resolve(ui.style());
        let w = |t: String| ui.ctx().fonts_mut(|f| f.layout_no_wrap(t, font.clone(), egui::Color32::WHITE)).size().x;
        // The padding the rows draw the text with (see the row painting), plus a pixel.
        Self {
            size: (w("1023.9 MiB".into()) + 9.0).max(MIN_SIZE_W),
            modified: (w(format_mtime(Some(std::time::SystemTime::now()), date_format)) + 5.0).max(MIN_MODIFIED_W),
        }
    }
}

impl Cols {
    fn new(rect: egui::Rect, name_w: f32, size_w: f32, need: ColNeed) -> Self {
        let name_left = rect.min.x + 4.0;
        let right = rect.max.x - 8.0;
        let avail = right - name_left;

        // Progressive disclosure. A pane can get far narrower than the three minimums add up
        // to, four panes beside a wide preview panel leave ~80px each. The old code forced
        // the minimums anyway, which pushed Size and Modified PAST the pane's right edge,
        // where they painted on top of the neighbouring pane. Drop Modified first, then Size.
        // Name is never dropped: a file list without names is useless.
        let show_modified = avail >= NAME_BEFORE_MODIFIED + need.size + need.modified;
        let show_size = avail >= MIN_NAME_W + need.size;

        if !show_size {
            return Self {
                name_left,
                name_w: avail.max(0.0),
                size_left: right,
                size_w: 0.0,
                modified_left: right,
                modified_w: 0.0,
                right,
                show_size: false,
                show_modified: false,
            };
        }

        // Keep the pair inside the pane however narrow it gets.
        let reserved = if show_modified { need.size + need.modified } else { need.size };
        let name_w = name_w.clamp(MIN_NAME_W, (avail - reserved).max(MIN_NAME_W));
        let size_room = if show_modified { avail - name_w - need.modified } else { avail - name_w };
        let size_w = size_w.clamp(need.size, size_room.max(need.size));

        let size_left = name_left + name_w;
        let modified_left = size_left + size_w;
        Self {
            name_left,
            name_w,
            size_left,
            size_w,
            // With Modified hidden, Size runs to the pane edge, callers right-align Size
            // against `modified_left`, so this keeps that arithmetic correct unchanged.
            modified_left: if show_modified { modified_left } else { right },
            modified_w: if show_modified { (right - modified_left).max(0.0) } else { 0.0 },
            right,
            show_size,
            show_modified,
        }
    }
}

/// ctrl+scroll zooms the icon size and, past the threshold, flips list <-> grid.
///
/// Read via `zoom_delta()`, NOT the scroll delta: when the zoom modifier is held, egui moves
/// the wheel into `zoom_factor_delta` and leaves `smooth_scroll_delta` at zero
/// (input_state/mod.rs:461), so reading scroll gave a constant zero and the gesture did
/// nothing. `zoom_delta()` is the blessed accessor and also picks up trackpad pinch for free.
fn handle_zoom(ui: &egui::Ui, pane: &mut Pane, rect: egui::Rect) -> bool {
    use crate::panes::{MAX_ZOOM, MIN_ZOOM};
    if !ui.rect_contains_pointer(rect) {
        return false;
    }
    let factor = ui.input(|i| i.zoom_delta());
    if (factor - 1.0).abs() < 1e-4 {
        return false;
    }
    // egui's own step is tuned for scaling a whole UI, where small increments are wanted.
    // Measured with a synthetic wheel: six notches moved the raw factor only ~1.26x total, so
    // climbing from list size to the grid threshold would have taken roughly sixty notches,
    // which reads as "nothing happens". Calibrated against the same rig: gain 6 turns six
    // notches into about 4x (16px -> ~67px), so a few notches cross into grid without
    // slamming into the 256px ceiling.
    const ZOOM_GAIN: f32 = 2.0;
    let old = pane.zoom;
    // Multiplicative, so a notch feels the same at every size.
    pane.zoom = (pane.zoom * factor.powf(ZOOM_GAIN)).clamp(MIN_ZOOM, MAX_ZOOM);
    if std::env::var_os("VANTAEXPLORER_ZOOM_DEBUG").is_some() && pane.zoom != old {
        log::info!("zoom {old:.1} -> {:.1} (grid={})", pane.zoom, pane.is_grid());
    }
    pane.zoom != old
}

fn show_list(
    ui: &mut egui::Ui,
    pane: &mut Pane,
    pane_idx: usize,
    is_active: bool,
    icons: &mut IconCache,
    view: &View,
) -> bool {
    slide_list(ui, pane, pane_idx, view.settings.animations, |ui, pane| {
        let zoomed = handle_zoom(ui, pane, ui.available_rect_before_wrap());
        if pane.is_grid() {
            return show_grid(ui, pane, pane_idx, is_active, icons, view) || zoomed;
        }
        show_rows_view(ui, pane, pane_idx, is_active, icons, view) || zoomed
    })
}

/// How long a folder takes to slide in. Short and fast to start, slowing into place: the
/// motion is there to show which way you went, not to be watched.
const SLIDE: std::time::Duration = std::time::Duration::from_millis(170);
/// How long a jump elsewhere takes to fade over.
const JUMP: std::time::Duration = std::time::Duration::from_millis(140);

/// How the list changes when its folder does.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Motion {
    /// Along the path: 1 the new folder comes in from the right (into a folder), -1 from the
    /// left (out to a parent).
    Slide(f32),
    /// Somewhere else altogether: the old list fades out shrinking a little, the new one fades
    /// in from a little larger. Neither sideways, which means along the path, nor up or down,
    /// which looks like scrolling.
    Jump,
}

/// A pane's list as it was last drawn, and the motion under way.
#[derive(Clone, Default)]
struct Slide {
    /// The tab and folder drawn last frame.
    at: Option<(usize, PathBuf)>,
    /// What the list painted last frame, to move out when the folder changes.
    last: std::sync::Arc<Vec<egui::epaint::ClippedShape>>,
    /// The old list when the motion began, when it began, and which motion.
    running: Option<(std::sync::Arc<Vec<egui::epaint::ClippedShape>>, std::time::Instant, Motion)>,
}

/// Down the path slides in from the right, up it from the left, and anything else is a jump.
/// Back and Forward follow from that: back out of a folder you went into slides left again,
/// back from a jump jumps.
fn motion(from: &std::path::Path, to: &std::path::Path) -> Motion {
    if to.starts_with(from) {
        Motion::Slide(1.0)
    } else if from.starts_with(to) {
        Motion::Slide(-1.0)
    } else {
        Motion::Jump
    }
}

/// Fades a shape to `a` of its opacity.
fn fade(shape: &mut egui::Shape, a: f32) {
    use egui::epaint::ColorMode;
    match shape {
        egui::Shape::Vec(v) => v.iter_mut().for_each(|s| fade(s, a)),
        egui::Shape::Circle(c) => {
            c.fill = c.fill.gamma_multiply(a);
            c.stroke.color = c.stroke.color.gamma_multiply(a);
        }
        egui::Shape::Ellipse(e) => {
            e.fill = e.fill.gamma_multiply(a);
            e.stroke.color = e.stroke.color.gamma_multiply(a);
        }
        egui::Shape::LineSegment { stroke, .. } => stroke.color = stroke.color.gamma_multiply(a),
        egui::Shape::Path(p) => {
            p.fill = p.fill.gamma_multiply(a);
            if let ColorMode::Solid(c) = &mut p.stroke.color {
                *c = c.gamma_multiply(a);
            }
        }
        egui::Shape::Rect(r) => {
            r.fill = r.fill.gamma_multiply(a);
            r.stroke.color = r.stroke.color.gamma_multiply(a);
        }
        egui::Shape::Text(t) => t.opacity_factor *= a,
        egui::Shape::Mesh(m) => std::sync::Arc::make_mut(m).vertices.iter_mut().for_each(|v| v.color = v.color.gamma_multiply(a)),
        _ => {}
    }
}

/// Draws a pane's list with `draw`, moving it in when the tab's folder has changed: along the
/// path the new list comes in from one side and pushes a picture of the old one out of the
/// other; a jump elsewhere fades from the one to the other (`Motion`). Only the picture moves:
/// the new list takes clicks and keys where it will stand from the first frame, so nothing
/// waits for the motion. The old picture is what the list painted the frame before, kept every
/// frame, so it is exactly what was on screen.
fn slide_list<R>(ui: &mut egui::Ui, pane: &mut Pane, pane_idx: usize, enabled: bool, draw: impl FnOnce(&mut egui::Ui, &mut Pane) -> R) -> R {
    let ctx = ui.ctx().clone();
    let id = egui::Id::new(("list-slide", pane_idx));
    let mut st: Slide = ctx.data(|d| d.get_temp(id)).unwrap_or_default();
    let here = (pane.active_tab, pane.tab_ref().path().to_path_buf());
    if let Some((t, from)) = &st.at {
        if enabled && *t == here.0 && *from != here.1 && !st.last.is_empty() {
            st.running = Some((st.last.clone(), std::time::Instant::now(), motion(from, &here.1)));
        }
    }
    st.at = Some(here);
    if !enabled {
        st.running = None;
    }

    let area = ui.available_rect_before_wrap().intersect(ui.clip_rect());
    let layer = ui.layer_id();
    let start = ctx.graphics_mut(|g| g.entry(layer).next_idx());
    let out = draw(ui, pane);
    let end = ctx.graphics_mut(|g| g.entry(layer).next_idx());
    // Kept only while it can be needed: the picture costs a copy of the frame's shapes.
    if enabled {
        st.last = std::sync::Arc::new(ctx.graphics_mut(|g| g.entry(layer).all_entries().skip(start.0).take(end.0 - start.0).cloned().collect()));
    }

    if let Some((old, t0, how)) = st.running.clone() {
        let length = if how == Motion::Jump { JUMP } else { SLIDE };
        let p = (t0.elapsed().as_secs_f32() / length.as_secs_f32()).min(1.0);
        if p >= 1.0 {
            st.running = None;
        } else {
            // Ease out: most of the way at once, then settling.
            let e = 1.0 - (1.0 - p).powi(3);
            // Where each list is drawn and how strongly: (transform, opacity) for new and old.
            let (new, was) = match how {
                Motion::Slide(way) => {
                    let new_dx = way * area.width() * (1.0 - e);
                    let at = |dx: f32| egui::emath::TSTransform::from_translation(egui::vec2(dx, 0.0));
                    ((at(new_dx), 1.0), (at(new_dx - way * area.width()), 1.0))
                }
                Motion::Jump => {
                    let c = area.center().to_vec2();
                    let scaled = |k: f32| egui::emath::TSTransform::new(c * (1.0 - k), k);
                    ((scaled(1.04 - 0.04 * e), e), (scaled(1.0 - 0.04 * e), 1.0 - e))
                }
            };
            let place = |c: &mut egui::epaint::ClippedShape, (t, a): (egui::emath::TSTransform, f32)| {
                c.shape.transform(t);
                if a < 1.0 {
                    fade(&mut c.shape, a);
                }
                c.clip_rect = t.mul_rect(c.clip_rect).intersect(area);
            };
            ctx.graphics_mut(|g| {
                let list = g.entry(layer);
                for i in start.0..end.0 {
                    list.mutate_shape(egui::layers::ShapeIdx(i), |c| place(c, new));
                }
                for c in old.iter() {
                    let mut c = c.clone();
                    place(&mut c, was);
                    list.add(c.clip_rect, c.shape);
                }
            });
            ctx.request_repaint();
        }
    }
    ctx.data_mut(|d| d.insert_temp(id, st));
    out
}

#[cfg(test)]
mod motion_tests {
    use super::{motion, Motion};
    use std::path::Path;

    #[test]
    fn along_the_path_slides_and_elsewhere_jumps() {
        assert_eq!(motion(Path::new("/home/w"), Path::new("/home/w/Documents/Arkiv")), Motion::Slide(1.0));
        assert_eq!(motion(Path::new("/home/w/Documents/Arkiv"), Path::new("/home/w")), Motion::Slide(-1.0));
        assert_eq!(motion(Path::new("/home/w/Documents"), Path::new("/home/w/Pictures")), Motion::Jump);
        assert_eq!(motion(Path::new("/home/w"), Path::new("/etc")), Motion::Jump);
    }
}

/// How long a marking takes to glide to its new place: quick enough that a held arrow key
/// never leaves it trailing far behind.
const GLIDE: std::time::Duration = std::time::Duration::from_millis(90);

/// How far along a glide begun at `t0` is, eased out like the folder slide; 1 once arrived.
fn glide_progress(t0: std::time::Instant) -> f32 {
    let p = (t0.elapsed().as_secs_f32() / GLIDE.as_secs_f32()).min(1.0);
    1.0 - (1.0 - p).powi(3)
}

/// The marking in a pane on its way between entries.
#[derive(Clone)]
struct Glide {
    /// The tab, folder and entry it was last on, and that entry's place (column, row).
    on: (usize, PathBuf, String, egui::Vec2),
    /// The place it set out from, and when; `None` once it has arrived.
    moving: Option<(egui::Vec2, std::time::Instant)>,
}

impl Glide {
    /// Where its fill stands now.
    fn shown(&self) -> egui::Vec2 {
        match self.moving {
            Some((from, t0)) => from + (self.on.3 - from) * glide_progress(t0),
            None => self.on.3,
        }
    }
}

/// Where a pane's marking stands this frame, in cells (column, row) of a layout `cols` wide,
/// and the entry it is heading for: `None` when it is standing still and the entries paint it
/// themselves. Only a single marked entry glides, and only when the marking moved to another
/// entry in the same folder; a new sort, filter or width moves the entry under it, which is not
/// the marking moving.
fn glide_entry(ctx: &egui::Context, pane_idx: usize, tab_idx: usize, tab: &crate::panes::Tab, visible: &[usize], cols: usize, enabled: bool) -> Option<(egui::Vec2, usize)> {
    let id = egui::Id::new(("entry-glide", pane_idx));
    let prev: Option<Glide> = ctx.data(|d| d.get_temp(id));
    let one = (tab.selection.len() == 1).then(|| tab.selection.iter().next()).flatten();
    let vi = one.and_then(|name| visible.iter().position(|&ei| tab.listing.entries.get(ei).is_some_and(|e| &e.name == name)));
    let (Some(name), Some(vi), true) = (one, vi, enabled) else {
        ctx.data_mut(|d| d.remove::<Glide>(id));
        return None;
    };
    let cols = cols.max(1);
    let place = egui::vec2((vi % cols) as f32, (vi / cols) as f32);
    let on = (tab_idx, tab.path().to_path_buf(), name.clone(), place);
    let mut g = match prev {
        // From wherever it stood last frame, so a new target halfway never jumps.
        Some(g) if g.on.0 == on.0 && g.on.1 == on.1 && g.on.2 != on.2 => Glide { moving: Some((g.shown(), std::time::Instant::now())), on },
        Some(g) if g.on == on => g,
        _ => Glide { on, moving: None },
    };
    let mut out = None;
    if let Some((_, t0)) = g.moving {
        if t0.elapsed() >= GLIDE {
            g.moving = None;
        } else {
            out = Some((g.shown(), vi));
            ctx.request_repaint();
        }
    }
    ctx.data_mut(|d| d.insert_temp(id, g));
    out
}

/// Whether there is motion this frame (`Settings::animations`), for the parts drawn far from
/// the settings: put there by `app_ui` every frame.
pub const ANIMATIONS: &str = "vx-animations";

/// Whether there is motion (`ANIMATIONS`).
pub fn animations(ctx: &egui::Context) -> bool {
    ctx.data(|d| d.get_temp::<bool>(egui::Id::new(ANIMATIONS))).unwrap_or(true)
}

/// What `glide_rect` keeps: the target, where it set out from and when, and the pass it was
/// last drawn in.
type GlideState = (egui::Rect, Option<(egui::Rect, std::time::Instant)>, u64);

/// A marking that is not tied to a list's cells (the sidebar's, the palette's, a question's
/// buttons), gliding to `to`. `to` is relative to what it scrolls with, so scrolling never
/// sets it moving. Returns where to draw it this frame. One not drawn the pass before (a
/// dialog or palette just opened) appears in place.
pub(crate) fn glide_rect(ctx: &egui::Context, id: egui::Id, to: egui::Rect, enabled: bool) -> egui::Rect {
    let pass = ctx.cumulative_pass_nr();
    let shown = |g: &GlideState| match g.1 {
        Some((from, t0)) => from.lerp_towards(&g.0, glide_progress(t0)),
        None => g.0,
    };
    let mut g: GlideState = match ctx.data(|d| d.get_temp::<GlideState>(id)) {
        Some(g) if enabled && g.2 + 1 >= pass && g.0 != to => (to, Some((shown(&g), std::time::Instant::now())), pass),
        Some(g) if enabled && g.2 + 1 >= pass => (g.0, g.1, pass),
        _ => (to, None, pass),
    };
    if g.1.is_some_and(|(_, t0)| t0.elapsed() >= GLIDE) {
        g.1 = None;
    }
    let at = shown(&g);
    if g.1.is_some() {
        ctx.request_repaint();
    }
    ctx.data_mut(|d| d.insert_temp(id, g));
    at
}

/// Forgets a `glide_rect` marking, so it appears where it next is instead of gliding there.
pub(crate) fn glide_forget(ctx: &egui::Context, id: egui::Id) {
    ctx.data_mut(|d| d.remove::<GlideState>(id));
}

/// A row or column of choices whose marking glides between them (`glide_rect`). Begun before
/// the choices are added, it holds a place under them for the marking; the choices are framed
/// buttons with no fill of their own, their usual one painted in that place too, as the marking
/// must lie over it and under their text.
pub(crate) struct Glider {
    slot: egui::layers::ShapeIdx,
    origin: egui::Vec2,
    under: Vec<egui::Shape>,
    chosen: Option<egui::Rect>,
}

impl Glider {
    pub(crate) fn begin(ui: &egui::Ui) -> Self {
        Glider { slot: ui.painter().add(egui::Shape::Noop), origin: ui.min_rect().min.to_vec2(), under: Vec::new(), chosen: None }
    }

    /// A button added with `.fill(TRANSPARENT)`: its usual fill, hovered or not, goes under it.
    pub(crate) fn button(&mut self, ui: &egui::Ui, r: &egui::Response) {
        let v = ui.style().interact(r);
        self.under.push(egui::Shape::rect_filled(r.rect, v.corner_radius, v.weak_bg_fill));
    }

    /// Where the marking belongs.
    pub(crate) fn chosen(&mut self, rect: egui::Rect) {
        self.chosen = Some(rect);
    }

    /// Paints the marking where it has got to: `fill` under the choices, `ring` (if any) over
    /// them. `snap` puts it in place at once, for a marking moved by the mouse or by new
    /// contents rather than by the keys. Returns where it was drawn.
    pub(crate) fn finish(mut self, ui: &egui::Ui, id: egui::Id, enabled: bool, snap: bool, radius: f32, fill: egui::Color32) -> Option<egui::Rect> {
        if snap {
            glide_forget(ui.ctx(), id);
        }
        let at = match self.chosen {
            Some(r) => Some(glide_rect(ui.ctx(), id, r.translate(-self.origin), enabled).translate(self.origin)),
            None => {
                glide_forget(ui.ctx(), id);
                None
            }
        };
        if let Some(r) = at {
            self.under.push(egui::Shape::rect_filled(r, radius, fill));
        }
        ui.painter().set(self.slot, egui::Shape::Vec(self.under));
        at
    }
}

/// Arrow-key browsing on the active pane. `columns` is 1 for the list, the cell count per row
/// for the grid. Returns the visible-index to move to, if any, and whether Shift was held (the
/// range from the anchor grows or shrinks to it instead of the selection moving).
///
/// Gated on wants_keyboard_input so typing in the path field or a rename box never moves the
/// selection underneath the cursor.
fn keyboard_nav(
    ui: &egui::Ui,
    is_active: bool,
    visible_len: usize,
    current: Option<usize>,
    columns: usize,
    page: usize,
) -> Option<(usize, bool)> {
    if !is_active || visible_len == 0 || ui.ctx().text_edit_focused() {
        return None;
    }
    let (up, down, left, right, page_up, page_down, shift) = ui.input(|i| {
        (
            !i.modifiers.alt && i.key_pressed(egui::Key::ArrowUp),
            !i.modifiers.alt && i.key_pressed(egui::Key::ArrowDown),
            !i.modifiers.alt && i.key_pressed(egui::Key::ArrowLeft),
            !i.modifiers.alt && i.key_pressed(egui::Key::ArrowRight),
            !i.modifiers.ctrl && i.key_pressed(egui::Key::PageUp),
            !i.modifiers.ctrl && i.key_pressed(egui::Key::PageDown),
            i.modifiers.shift,
        )
    });
    // A page is what the view shows, and stops at the first or last entry rather than not
    // moving at all.
    if page_up || page_down {
        let from = current.unwrap_or(0) as isize;
        let step = (page.max(1) * columns) as isize;
        let to = if page_down { from + step } else { from - step };
        return Some((to.clamp(0, visible_len as isize - 1) as usize, shift && current.is_some()));
    }
    let step: isize = if down {
        columns as isize
    } else if up {
        -(columns as isize)
    } else if right && columns > 1 {
        1
    } else if left && columns > 1 {
        -1
    } else {
        return None;
    };
    let next = match current {
        Some(c) => {
            let n = c as isize + step;
            if n < 0 || n >= visible_len as isize {
                return None;
            }
            n as usize
        }
        // Nothing selected yet: the first press lands on the first entry.
        None => 0,
    };
    Some((next, shift && current.is_some()))
}

/// Where the keyboard is in the visible order: the moving end of a Shift range when there is
/// one for the current anchor, else the anchor.
fn keyboard_position(tab: &crate::panes::Tab, visible: &[usize]) -> Option<usize> {
    let at = match (&tab.select_cursor, &tab.select_anchor) {
        (Some((anchor, cursor)), Some(a)) if anchor == a => cursor,
        (_, Some(a)) => a,
        _ => return None,
    };
    visible.iter().position(|&i| tab.listing.entries[i].name == *at)
}

/// Selects everything from the anchor to `vi` in the visible order, the anchor staying where
/// it is, as Shift+arrow and Shift+click do.
fn select_range_to(tab: &mut crate::panes::Tab, visible: &[usize], vi: usize) {
    let Some(name) = visible.get(vi).and_then(|&ei| tab.listing.entries.get(ei)).map(|e| e.name.clone()) else { return };
    let anchor_vi = tab
        .select_anchor
        .as_ref()
        .and_then(|a| visible.iter().position(|&i| tab.listing.entries[i].name == *a))
        .unwrap_or(vi);
    let (lo, hi) = if anchor_vi <= vi { (anchor_vi, vi) } else { (vi, anchor_vi) };
    tab.selection.clear();
    for &i in &visible[lo..=hi] {
        if let Some(e) = tab.listing.entries.get(i) {
            tab.selection.insert(e.name.clone());
        }
    }
    let anchor = tab.select_anchor.clone().unwrap_or_else(|| name.clone());
    tab.select_anchor = Some(anchor.clone());
    tab.select_cursor = Some((anchor, name));
    tab.scroll_to = Some(vi);
}

/// Selected entries as absolute paths, in listing order.
fn selected_paths(tab: &crate::panes::Tab) -> Vec<std::path::PathBuf> {
    tab.listing
        .entries
        .iter()
        .filter(|e| tab.selection.contains(&e.name))
        .map(|e| e.path.clone())
        .collect()
}

/// A new folder (or empty file) in the tab's folder, selected and straight into rename, recorded
/// so Ctrl+Z trashes it again.
fn create_in_place(tab: &mut crate::panes::Tab, view: &View, folder: bool) {
    let here = tab.path().to_path_buf();
    let result = if folder {
        crate::fileops::new_folder(&here).map_err(|e| format!("new folder: {e}"))
    } else {
        crate::fileops::new_file(&here).map_err(|e| format!("new file: {e}"))
    };
    match result {
        Ok(path) => {
            let name = path.file_name().unwrap_or_default().to_string_lossy().into_owned();
            tab.reload();
            tab.selection.clear();
            tab.selection.insert(name.clone());
            tab.select_anchor = Some(name.clone());
            tab.renaming = Some((name.clone(), name));
            view.actions.borrow_mut().push(Action::RecordCreated(path));
        }
        Err(e) => {
            log::error!("{e}");
            view.actions.borrow_mut().push(Action::Toast(e));
        }
    }
}

/// Right-click convention: a click on an unselected item re-selects just it, so the menu
/// always applies to what is under the cursor.
fn fix_selection_for_context(tab: &mut crate::panes::Tab, name: &str) {
    if !tab.selection.contains(name) {
        tab.selection.clear();
        tab.selection.insert(name.to_string());
        tab.select_anchor = Some(name.to_string());
    }
}

/// The per-entry context menu, shared by list and grid.
fn entry_context_menu(
    ui: &mut egui::Ui,
    tab: &mut crate::panes::Tab,
    entry_name: &str,
    entry_is_dir: bool,
    view: &View,
) {
    let single = tab.selection.len() == 1;
    let paths = selected_paths(tab);
    let push = |a: Action| view.actions.borrow_mut().push(a);

    if crate::fileops::in_trash(tab.path()) {
        if view.chooser.is_none() {
            trash_context_menu(ui, paths, view);
        }
        return;
    }

    if ui.button("Open").clicked() {
        if entry_is_dir && single {
            let p = paths[0].clone();
            tab.navigate(p);
        } else {
            push(Action::Activate(paths.clone()));
        }
        ui.close();
    }
    // The chooser's menu ends here, with the one thing a save now and then needs.
    if view.chooser.is_some() {
        if single && ui.button("Rename").clicked() {
            tab.renaming = Some((entry_name.to_string(), entry_name.to_string()));
            ui.close();
        }
        return;
    }
    // Files only: a folder is opened here. One file or many, of one type or several; for
    // several, only the applications that take all of them are offered.
    let files_only = if single { !entry_is_dir } else { paths.iter().all(|p| tab.listing.entries.iter().find(|e| e.path == *p).is_some_and(|e| !e.is_dir)) };
    if !paths.is_empty() && files_only {
        let targets = paths.clone();
        submenu(ui, "Open with", |ui| {
            // Looked up once for the files the menu is open on, not on every frame it stays
            // open: it runs xdg-mime and reads the desktop files.
            thread_local! {
                static OPEN_WITH: std::cell::RefCell<Option<OfferedApps>> = const { std::cell::RefCell::new(None) };
            }
            let candidates = OPEN_WITH.with(|c| {
                let mut c = c.borrow_mut();
                // Looked up again once a default has been set, since the default goes first.
                let set = crate::apps::defaults_set();
                if c.as_ref().is_none_or(|(p, n, _)| *p != targets || *n != set) {
                    *c = Some((targets.clone(), set, crate::apps::candidates_for(&targets)));
                }
                c.as_ref().map(|(_, _, v)| v.clone()).unwrap_or_default()
            });
            if candidates.is_empty() {
                ui.weak(if targets.len() > 1 { "No application takes all of them" } else { "No applications found" });
            }
            for app in &candidates {
                if ui.button(&app.name).clicked() {
                    for t in &targets {
                        crate::model::log_recent(t);
                    }
                    crate::apps::launch_many(app, &targets);
                    ui.close();
                }
            }
            ui.separator();
            // Every installed application, and the place to make one the default for the type.
            if ui.button("Other application...").clicked() {
                push(Action::OpenWith(targets.clone()));
                ui.close();
            }
        });
    }
    ui.separator();
    if ui.button("Copy").clicked() {
        publish_paths(ui.ctx(), &paths);
        push(Action::SetClipboard { paths: paths.clone(), cut: false });
        ui.close();
    }
    // Compressing works on whatever is selected, files or folders, one or many.
    if !paths.is_empty() {
        let parent = paths[0]
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| tab.path().to_path_buf());
        submenu(ui, "Compress to", |ui| {
            // zstd first: it is the one worth reaching for by default. zip and 7z are here
            // because other people's machines expect them.
            for (label, suffix) in [
                ("tar.zst", "tar.zst"),
                ("tar.gz", "tar.gz"),
                ("zip", "zip"),
                ("7z", "7z"),
            ] {
                if ui.button(label).clicked() {
                    let mut req = crate::fileops::Request::new(
                        crate::fileops::OpKind::Compress,
                        paths.clone(),
                        Some(parent.clone()),
                    );
                    req.format = Some(suffix.to_string());
                    push(Action::Submit(req));
                    ui.close();
                }
            }
        });
    }

    // Archives: offered on a single archive file, since extracting several at once into the
    // same place is a good way to make a mess you cannot untangle.
    if single && !entry_is_dir && crate::fileops::is_archive(&paths[0]) {
        let archive = paths[0].clone();
        let parent = archive
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| tab.path().to_path_buf());
        let folder = crate::fileops::extract_dir_name(&archive);

        if ui
            .button(format!("Extract to \"{folder}\""))
            .on_hover_text("Unpack into a new folder beside the archive")
            .clicked()
        {
            push(Action::Submit(crate::fileops::Request::new(
                crate::fileops::OpKind::Extract,
                vec![archive.clone()],
                Some(parent.join(&folder)),
            )));
            ui.close();
        }
        if ui
            .button("Extract here")
            .on_hover_text("Unpack into this folder; existing files are never overwritten")
            .clicked()
        {
            push(Action::Submit(crate::fileops::Request::new(
                crate::fileops::OpKind::Extract,
                vec![archive],
                Some(parent),
            )));
            ui.close();
        }
        ui.separator();
    }

    {
        let dir = if single && entry_is_dir {
            paths[0].clone()
        } else {
            tab.path().to_path_buf()
        };
        if ui
            .button("Open Terminal Here")
            .on_hover_text(format!("{} (F4)", view.settings.terminal_command().map_or_else(|| "no terminal".to_string(), |(t, _)| t)))
            .clicked()
        {
            push(Action::OpenTerminal(dir));
            ui.close();
        }
    }

    if entry_is_dir && single {
        folder_view_menu(ui, &paths[0], view);
        add_to_sidebar_menu(ui, &paths[0], view);
    }
    if ui
        .button("Copy path")
        .on_hover_text("Absolute path as text (Ctrl+Alt+C)")
        .clicked()
    {
        publish_paths(ui.ctx(), &paths);
        ui.close();
    }
    if ui.button("Cut").clicked() {
        publish_paths(ui.ctx(), &paths);
        push(Action::SetClipboard { paths: paths.clone(), cut: true });
        ui.close();
    }
    if entry_is_dir
        && single
        && ui
            .add_enabled(view.clipboard_filled, egui::Button::new("Paste into folder"))
            .clicked()
    {
        push(Action::Paste { dest: paths[0].clone() });
        ui.close();
    }
    if single && ui.button("Rename").clicked() {
        tab.renaming = Some((entry_name.to_string(), entry_name.to_string()));
        ui.close();
    }
    ui.separator();
    if ui.button("Move to Trash").clicked() {
        push(Action::Submit(crate::fileops::Request::new(crate::fileops::OpKind::Trash, paths.clone(), None)));
        ui.close();
    }
    if ui
        .button(egui::RichText::new("Delete permanently").color(view.theme.error))
        .clicked()
    {
        push(Action::ConfirmDelete(paths.clone()));
        ui.close();
    }
    ui.separator();
    if ui.button("Properties").on_hover_text("Alt+Enter").clicked() {
        push(Action::Properties(paths));
        ui.close();
    }
}

/// The trash view's menu: nothing that would leave a payload without its .trashinfo (no
/// cut, rename or move), only what the trash is for.
fn trash_context_menu(ui: &mut egui::Ui, paths: Vec<PathBuf>, view: &View) {
    let push = |a: Action| view.actions.borrow_mut().push(a);
    if ui.button("Restore").on_hover_text("Put back where it was").clicked() {
        push(Action::Restore(paths.clone()));
        ui.close();
    }
    if ui
        .button(egui::RichText::new("Delete permanently").color(view.theme.error))
        .clicked()
    {
        push(Action::ConfirmDelete(paths));
        ui.close();
    }
    ui.separator();
    if ui
        .button(egui::RichText::new("Empty Trash").color(view.theme.error))
        .clicked()
    {
        push(Action::EmptyTrash);
        ui.close();
    }
}

/// "Add to sidebar", a submenu of Special Folders and the user's own sections. A section the
/// folder is already in is greyed out. This is also how a special folder that was removed from
/// the sidebar comes back.
fn add_to_sidebar_menu(ui: &mut egui::Ui, folder: &std::path::Path, view: &View) {
    if view.sidebar_targets.is_empty() {
        return;
    }
    submenu(ui, "Add to sidebar", |ui| {
        for t in view.sidebar_targets {
            let there = t.paths.iter().any(|p| p == folder);
            let resp = ui.add_enabled(!there, egui::Button::new(&t.title));
            if there {
                resp.on_disabled_hover_text("Already there");
            } else if resp.clicked() {
                view.actions.borrow_mut().push(Action::AddToSidebar { index: t.index, path: folder.to_path_buf() });
                ui.close();
            }
        }
    });
}

/// The applications "Open with" offered, for which files, as of which default being set.
type OfferedApps = (Vec<PathBuf>, u64, Vec<crate::apps::AppEntry>);

/// Background (empty area) context menu: acts on the folder itself.
fn folder_context_menu(ui: &mut egui::Ui, tab: &mut crate::panes::Tab, view: &View) {
    let here = tab.path().to_path_buf();
    if crate::fileops::in_trash(&here) {
        if view.chooser.is_some() {
            return;
        }
        let empty = tab.listing.entries.is_empty();
        if ui
            .add_enabled(
                !empty,
                egui::Button::new(egui::RichText::new("Empty Trash").color(view.theme.error)),
            )
            .clicked()
        {
            view.actions.borrow_mut().push(Action::EmptyTrash);
            ui.close();
        }
        return;
    }
    // Both create in place and drop straight into rename: nobody wants something actually
    // called "New File". Both are recorded so Ctrl+Z trashes what was just made.
    if ui.button("New Folder").on_hover_text("Ctrl+Shift+N").clicked() {
        create_in_place(tab, view, true);
        ui.close();
    }
    // A new folder is the one thing worth making while choosing where something goes.
    if view.chooser.is_some() {
        return;
    }
    if ui.button("New File").clicked() {
        create_in_place(tab, view, false);
        ui.close();
    }
    if ui.add_enabled(view.clipboard_filled, egui::Button::new("Paste")).clicked() {
        view.actions.borrow_mut().push(Action::Paste { dest: here.clone() });
        ui.close();
    }
    ui.separator();
    folder_view_menu(ui, &here, view);
    add_to_sidebar_menu(ui, &here, view);
    if ui
        .button("Copy path")
        .on_hover_text("Copy this folder's path to the clipboard")
        .clicked()
    {
        publish_paths(ui.ctx(), std::slice::from_ref(&here));
        ui.close();
    }
    if ui
        .button("Open Terminal Here")
        .on_hover_text(format!("{} (F4)", view.settings.terminal_command().map_or_else(|| "no terminal".to_string(), |(t, _)| t)))
        .clicked()
    {
        view.actions.borrow_mut().push(Action::OpenTerminal(here.clone()));
        ui.close();
    }
    ui.separator();
    if ui.button("Properties").on_hover_text("Of this folder (Alt+Enter with nothing marked)").clicked() {
        view.actions.borrow_mut().push(Action::Properties(vec![here]));
        ui.close();
    }
}

/// Clipboard/trash/rename keyboard shortcuts for the active pane.
/// The selection's absolute paths, one per line, what "Copy path" puts on the clipboard.
fn paths_as_text(paths: &[std::path::PathBuf]) -> String {
    paths
        .iter()
        .map(|p| p.to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("\n")
}

/// Put the paths on the system clipboard as text.
///
/// Not a convenience: egui-winit only emits `Event::Paste` when the system clipboard actually
/// holds text, so without this a file copy would leave the clipboard empty and the following
/// Ctrl+V would produce no event at all. Pasting the paths into a terminal is a useful
/// side effect rather than the point.
fn publish_paths(ctx: &egui::Context, paths: &[std::path::PathBuf]) {
    let text = paths_as_text(paths);
    if !text.is_empty() {
        ctx.copy_text(text);
    }
}

fn handle_shortcuts(
    ui: &egui::Ui,
    tab: &mut crate::panes::Tab,
    view: &View,
    visible: &[usize],
) -> bool {
    // Suppress shortcuts only while a TEXT FIELD has focus (rename box, path edit).
    //
    // This used to test `memory().focused().is_some()`, which is far broader: egui hands focus
    // to any clickable widget, so simply clicking a row focused it and killed every shortcut
    // in the pane, Ctrl+C did nothing after selecting a file with the mouse. Ctrl+Z appeared
    // to work only because reaching it via the context menu cleared focus on the way.
    // Ctrl+Alt+C, seen as itself by main.rs: taken whatever happens next, so it never lingers.
    let copy_path_key = ui.ctx().data_mut(|d| d.remove_temp::<bool>(egui::Id::new(COPY_PATH_KEY))).unwrap_or(false);
    if ui.ctx().text_edit_focused() || view.modal_open {
        return false;
    }
    let paths = selected_paths(tab);
    let push = |a: Action| view.actions.borrow_mut().push(a);
    // egui-winit INTERCEPTS Ctrl+C/X/V: it pushes Event::Copy/Cut/Paste and returns before
    // ever emitting Event::Key. So `key_pressed(Key::C)` can never be true for Ctrl+C, and
    // testing for it silently does nothing. Ctrl+Z is not intercepted, which is the whole
    // reason undo appeared to work while copy did not.
    //
    // Event::Copy therefore means "copy the files", unconditionally. It used to be shared
    // with Ctrl+Alt+C (copy path), told apart by `i.modifiers.alt` at that instant, but
    // `is_copy_command` ignores BOTH alt and shift, so every Ctrl+C variant arrives here
    // identical and that modifier flag was the only thing separating them. A stale alt (a
    // modifier held while focus left the window, e.g. from a compositor shortcut) turned every Ctrl+C into
    // a silent "copy path": the file clipboard was never filled, so paste had nothing to do.
    // Copy path now has its own, non-intercepted key.
    let (copy, cut, paste) = ui.input(|i| {
        let mut copy = false;
        let mut cut = false;
        let mut paste = false;
        for e in &i.events {
            match e {
                egui::Event::Copy => copy = true,
                egui::Event::Cut => cut = true,
                egui::Event::Paste(_) => paste = true,
                _ => {}
            }
        }
        (copy, cut, paste)
    });
    // Ctrl+Shift+P is the command palette (palette.rs).
    let copy_path = copy_path_key;
    // The Copy event the same keystroke made is not a file copy.
    let copy = copy && !copy_path_key;
    if std::env::var_os("VANTAEXPLORER_KEY_DEBUG").is_some()
        && (copy || cut || paste || copy_path || view.paste_key)
    {
        let m = ui.input(|i| i.modifiers);
        log::info!(
            "shortcut: copy={copy} cut={cut} paste_event={paste} paste_key={} copy_path={copy_path} | ctrl={} alt={} shift={} | selection={} clipboard_filled={}",
            view.paste_key, m.ctrl, m.alt, m.shift, paths.len(), view.clipboard_filled
        );
    }
    // Cut, paste, rename and Delete-to-trash make no sense inside the trash: they would
    // strand payloads without their .trashinfo (or trash the trash).
    let in_trash = crate::fileops::in_trash(tab.path());
    let (del, shift_del, f2, enter, select_all) = ui.input(|i| {
        (
            !i.modifiers.shift && i.key_pressed(egui::Key::Delete),
            i.modifiers.shift && i.key_pressed(egui::Key::Delete),
            i.key_pressed(egui::Key::F2),
            i.key_pressed(egui::Key::Enter),
            // Not intercepted by egui-winit the way Ctrl+C/X/V are, so a plain key test works.
            i.modifiers.ctrl && !i.modifiers.alt && i.key_pressed(egui::Key::A),
        )
    });
    // The chooser keeps what finds and names a file (Enter, F2, a new folder, select all) and
    // drops what moves, removes or leaves the window.
    let choosing = view.chooser.is_some();
    let (copy, cut, paste, copy_path, del, shift_del) = if choosing { (false, false, false, false, false, false) } else { (copy, cut, paste, copy_path, del, shift_del) };
    // Alt+Enter: what is marked, or the folder itself when nothing is.
    if !choosing && ui.input(|i| i.modifiers.alt && i.key_pressed(egui::Key::Enter)) {
        let what = if paths.is_empty() { vec![tab.path().to_path_buf()] } else { paths.clone() };
        if !crate::fileops::in_trash(tab.path()) || !paths.is_empty() {
            push(Action::Properties(what));
        }
        return true;
    }
    if !choosing && ui.input(|i| i.key_pressed(egui::Key::F4)) {
        // Dolphin's binding. The terminal opens in the folder you are looking at.
        push(Action::OpenTerminal(tab.path().to_path_buf()));
        return true;
    }
    if ui.input(|i| i.modifiers.ctrl && i.modifiers.shift && i.key_pressed(egui::Key::N)) && !in_trash {
        create_in_place(tab, view, true);
        return true;
    }
    // Home and End (Alt+Home is the home folder, handled with the navigation keys); with Shift
    // the range from the anchor reaches the first or last entry.
    let (home, end, shift) = ui.input(|i| (!i.modifiers.alt && i.key_pressed(egui::Key::Home), !i.modifiers.alt && i.key_pressed(egui::Key::End), i.modifiers.shift));
    if !visible.is_empty() && (home || end) {
        let vi = if home { 0 } else { visible.len() - 1 };
        if shift {
            select_range_to(tab, visible, vi);
        } else {
            select_index(tab, visible, vi);
        }
        return true;
    }
    let mut used = true;
    if select_all && !visible.is_empty() {
        // Everything VISIBLE, not everything in the folder: with a filter active, selecting
        // all should mean what is on screen, or you would act on files you cannot see.
        tab.selection.clear();
        for &ei in visible {
            if let Some(e) = tab.listing.entries.get(ei) {
                tab.selection.insert(e.name.clone());
            }
        }
        // Anchor at the end so a following shift+arrow extends from there rather than
        // collapsing back to wherever the cursor happened to be.
        tab.select_anchor = visible
            .last()
            .and_then(|&ei| tab.listing.entries.get(ei))
            .map(|e| e.name.clone());
    } else if copy_path && !paths.is_empty() {
        publish_paths(ui.ctx(), &paths);
    } else if copy && !paths.is_empty() {
        publish_paths(ui.ctx(), &paths);
        push(Action::SetClipboard { paths, cut: false });
    } else if cut && !paths.is_empty() && !in_trash {
        publish_paths(ui.ctx(), &paths);
        push(Action::SetClipboard { paths, cut: true });
    } else if (paste || (view.paste_key && !choosing)) && view.clipboard_filled && !in_trash {
        push(Action::Paste { dest: tab.path().to_path_buf() });
    } else if del && !paths.is_empty() && !in_trash {
        push(Action::Submit(crate::fileops::Request::new(crate::fileops::OpKind::Trash, paths, None)));
    } else if (shift_del || del) && !paths.is_empty() {
        // In the trash view a plain Delete is already the permanent one, with the same
        // confirmation, since there is no further net below.
        push(Action::ConfirmDelete(paths));
    } else if f2 && tab.selection.len() == 1 && !in_trash {
        let name = tab.selection.iter().next().unwrap().clone();
        tab.renaming = Some((name.clone(), name));
    } else if enter && !paths.is_empty() {
        if tab.selection.len() == 1 && paths[0].is_dir() {
            let p = paths[0].clone();
            tab.navigate(p);
        } else {
            push(Action::Activate(paths));
        }
    } else {
        used = false;
    }
    used
}

/// Inline rename editor placed over the given rect. Returns true while active.
fn rename_editor(
    ui: &mut egui::Ui,
    tab: &mut crate::panes::Tab,
    rect: egui::Rect,
    view: &View,
) -> bool {
    let here = tab.path().to_path_buf();
    let Some((original, buf)) = &mut tab.renaming else { return false };
    let original = original.clone();
    let resp = ui.put(rect, egui::TextEdit::singleline(buf).margin(egui::vec2(2.0, 0.0)));

    let escape = ui.input(|i| i.key_pressed(egui::Key::Escape));
    let entered = resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
    // Clicking away KEEPS the typed name. Half-finished renaming is almost always "I typed it
    // and moved on", not "I meant to throw it away", and losing the name to a stray click
    // costs more than the rare unwanted rename, which Ctrl+Z reverses anyway. Escape is the
    // way to abandon. (Escape also surrenders focus, so it must be tested first.)
    let clicked_away = resp.lost_focus() && !entered;

    if escape {
        tab.renaming = None;
    } else if entered || clicked_away {
        let new_name = buf.trim().to_string();
        tab.renaming = None;
        if !new_name.is_empty() && new_name != original {
            let src = tab.path().join(&original);
            log::debug!("rename '{original}' to '{new_name}' in {}", here.display());
            match crate::fileops::rename(&src, &new_name) {
                Ok(dest) => {
                    view.actions
                        .borrow_mut()
                        .push(Action::RecordRename { from: src, to: dest });
                    tab.reload();
                    tab.selection.clear();
                    tab.selection.insert(new_name.clone());
                    tab.select_anchor = Some(new_name);
                }
                Err(e) => {
                    // Now that a click commits, a failure is no longer something the user
                    // was watching for, say it out loud rather than only in the log.
                    log::error!("rename: {e}");
                    view.actions
                        .borrow_mut()
                        .push(Action::Toast(format!("Could not rename '{original}': {e}")));
                }
            }
        }
    } else if !resp.has_focus() {
        // Only on the first frame the editor exists; see clicked_away above.
        resp.request_focus();
        // Open with the name selected, so typing replaces it: all of a folder's, and a file's
        // up to its extension, which is almost never what is being changed.
        let chars = buf.chars().count();
        let is_dir = here.join(&original).is_dir();
        let stem = if is_dir {
            chars
        } else {
            buf.rfind('.').filter(|&i| i > 0).map_or(chars, |i| buf[..i].chars().count())
        };
        let mut st = egui::TextEdit::load_state(ui.ctx(), resp.id).unwrap_or_default();
        st.cursor.set_char_range(Some(egui::text::CCursorRange::two(egui::text::CCursor::new(0), egui::text::CCursor::new(stem))));
        st.store(ui.ctx(), resp.id);
    }
    true
}

/// Apply a keyboard selection: single-select, re-anchor, and reveal.
/// Drag a rectangle across empty space to select everything it touches.
///
/// The band rides on the list/grid *background* response rather than an overlay of its own.
/// Rows and cells sense drag themselves for drag-and-drop and are registered later, so egui
/// hands the drag to the background only where no item sits under the pointer, which is
/// exactly the Dolphin rule (drag a file = move it, drag empty space = select).
///
/// The start point is kept in CONTENT coordinates, so if the view scrolls mid-drag the band
/// stays anchored to the files it started from instead of sliding with the viewport.
///
/// `for_each_hit` reports the visible indices a band rect touches. It takes the rect rather
/// than returning every index so a 100k-entry listing costs O(hits), not O(entries), per frame.
struct BandView {
    /// The scroll area's visible rect, in screen coordinates.
    viewport: egui::Rect,
    scroll_y: f32,
    accent: egui::Color32,
}

fn rubber_band(
    ui: &egui::Ui,
    bg: &egui::Response,
    v: &BandView,
    tab: &mut crate::panes::Tab,
    visible: &[usize],
    for_each_hit: impl FnOnce(egui::Rect, &mut dyn FnMut(usize)),
) -> bool {
    let BandView { viewport, scroll_y, accent } = *v;
    let id = bg.id.with("band");
    type Band = (egui::Pos2, bool);

    if bg.drag_started_by(egui::PointerButton::Primary) {
        if let Some(p) = bg.interact_pointer_pos() {
            // ctrl/shift keep what was already selected, so a band can extend a selection.
            let additive = ui.input(|i| i.modifiers.ctrl || i.modifiers.shift);
            ui.data_mut(|d| d.insert_temp::<Band>(id, (egui::pos2(p.x, p.y + scroll_y), additive)));
        }
    }

    let Some((start_content, additive)) = ui.data(|d| d.get_temp::<Band>(id)) else {
        return false;
    };
    // Also drop the anchor whenever the button is simply not down. Relying on drag_stopped()
    // alone strands a stale start point if the release lands outside the window (or the app
    // loses focus mid-drag), and the NEXT band then rubber-bands from that old anchor, a
    // huge, wrong selection from a small drag.
    if bg.drag_stopped() || !ui.input(|i| i.pointer.primary_down()) {
        ui.data_mut(|d| d.remove_temp::<Band>(id));
        return false;
    }
    if !bg.dragged_by(egui::PointerButton::Primary) {
        return false;
    }
    let Some(cur) = bg.interact_pointer_pos() else {
        return false;
    };

    let start = egui::pos2(start_content.x, start_content.y - scroll_y);
    let band = egui::Rect::from_two_pos(start, cur).intersect(viewport);

    let mut hits = Vec::new();
    for_each_hit(band, &mut |vi| hits.push(vi));
    if !additive {
        tab.selection.clear();
    }
    for vi in hits {
        if let Some(e) = visible.get(vi).and_then(|&ei| tab.listing.entries.get(ei)) {
            tab.selection.insert(e.name.clone());
        }
    }

    ui.painter().rect_filled(band, 1.0, accent.gamma_multiply(0.18));
    ui.painter()
        .rect_stroke(band, 1.0, egui::Stroke::new(1.0, accent), egui::StrokeKind::Inside);
    true
}

/// A folder just opened has its first entry marked once all of it has arrived, so the arrows
/// and Enter work at once. Not over a folder to reveal (Left marks the one just left), not
/// once something is marked by hand, and not when the chooser saves: marking a file there
/// takes its name.
fn mark_first(tab: &mut crate::panes::Tab, visible: &[usize], view: &View) {
    if !tab.mark_first || tab.rx.is_some() {
        return;
    }
    tab.mark_first = false;
    let saving = view.chooser.is_some_and(|c| c.request.mode == crate::chooser::Mode::Save);
    if tab.pending_reveal.is_none() && tab.selection.is_empty() && !saving {
        select_index(tab, visible, 0);
    }
}

fn select_index(tab: &mut crate::panes::Tab, visible: &[usize], vi: usize) {
    let Some(name) = visible
        .get(vi)
        .and_then(|&ei| tab.listing.entries.get(ei))
        .map(|e| e.name.clone())
    else {
        return;
    };
    tab.selection.clear();
    tab.selection.insert(name.clone());
    tab.select_anchor = Some(name);
    tab.scroll_to = Some(vi);
}

fn show_rows_view(
    ui: &mut egui::Ui,
    pane: &mut Pane,
    pane_idx: usize,
    is_active: bool,
    icons: &mut IconCache,
    view: &View,
) -> bool {
    let theme = view.theme;
    let settings = view.settings;
    let show_hidden = pane.show_hidden;
    let tab_idx = pane.active_tab;
    let (pane_col_name, pane_col_size) = (pane.col_name_w, pane.col_size_w);
    // List rows scale with the zoom as well; otherwise ctrl+scroll would appear to do nothing
    // until the pane flipped to grid.
    let icon_sz = pane.zoom;
    let text_h = ui.text_style_height(&egui::TextStyle::Body);
    let row_h = (icon_sz + 6.0).max(text_h + 4.0);
    // Collected during the header pass and written back to the pane at the end.
    let mut col_drag = (pane_col_name, pane_col_size);
    let tab = pane.tab();
    let mut interacted = false;

    // --- header: painter-drawn over the same column geometry the rows use ---
    let label_of = |k: SortKey, _spec: crate::model::SortSpec| {
        let base = match k {
            SortKey::Name => "Name",
            SortKey::Size => "Size",
            SortKey::Modified => "Modified",
        };
        base.to_string()
    };
    let mut resort: Option<SortKey> = None;
    let need = ColNeed::measure(ui, &settings.date_format);
    {
        let (hrect, _) = ui.allocate_exact_size(
            egui::vec2(ui.available_width(), HEADER_H),
            egui::Sense::hover(),
        );
        let cols = Cols::new(hrect, pane_col_name, pane_col_size, need);
        let spec = tab.listing.sort;
        let font = egui::TextStyle::Body.resolve(ui.style());
        let col = theme.text.gamma_multiply(0.7);

        let name_rect = egui::Rect::from_min_max(
            egui::pos2(cols.name_left, hrect.min.y),
            egui::pos2(cols.size_left, hrect.max.y),
        );
        let size_rect = egui::Rect::from_min_max(
            egui::pos2(cols.size_left, hrect.min.y),
            egui::pos2(cols.modified_left, hrect.max.y),
        );
        let mod_rect = egui::Rect::from_min_max(
            egui::pos2(cols.modified_left, hrect.min.y),
            egui::pos2(cols.right, hrect.max.y),
        );
        let mut header_cols = vec![(name_rect, SortKey::Name, egui::Align2::LEFT_CENTER)];
        // Size right-aligned so digits line up; Modified left-aligned so it sits right
        // beside Size once Name is dragged narrow.
        if cols.show_size {
            header_cols.push((size_rect, SortKey::Size, egui::Align2::RIGHT_CENTER));
        }
        if cols.show_modified {
            header_cols.push((mod_rect, SortKey::Modified, egui::Align2::LEFT_CENTER));
        }
        for (rect, key, align) in header_cols {
            let resp = ui.interact(rect, ui.id().with(("hdr", key as u8)), egui::Sense::click());
            if resp.hovered() {
                ui.painter().rect_filled(rect, 2.0, theme.accent.gamma_multiply(0.18));
            }
            if resp.clicked() {
                resort = Some(key);
            }
            let anchor = match align {
                egui::Align2::LEFT_CENTER => egui::pos2(rect.min.x + 2.0, rect.center().y),
                _ => egui::pos2(rect.max.x - 6.0, rect.center().y),
            };
            let galley = ui.painter().text(anchor, align, label_of(key, spec), font.clone(), col);
            // Sort direction as a painted triangle, theme fonts lack the arrow code points.
            if spec.key == key {
                let mark = egui::Rect::from_center_size(
                    egui::pos2(galley.right() + 8.0, rect.center().y),
                    egui::vec2(12.0, 12.0),
                );
                glyph::paint(
                    ui.painter(),
                    mark,
                    if spec.ascending { Glyph::Up } else { Glyph::ChevronDown },
                    theme.accent_text,
                );
            }
        }

        // Draggable dividers at the Name|Size and Size|Modified boundaries. Each resizes the
        // column to its LEFT, so everything to the right slides along with it.
        let mut boundaries: Vec<(f32, usize)> = Vec::new();
        if cols.show_size {
            boundaries.push((cols.size_left, 0usize));
        }
        if cols.show_modified {
            boundaries.push((cols.modified_left, 1usize));
        }
        for (bx, which) in boundaries {
            let handle = egui::Rect::from_min_max(
                egui::pos2(bx - 3.0, hrect.min.y),
                egui::pos2(bx + 3.0, hrect.max.y),
            );
            let hr = ui.interact(
                handle,
                ui.id().with(("colgrip", pane_idx, which)),
                egui::Sense::drag(),
            );
            if hr.hovered() || hr.dragged() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
                ui.painter().rect_filled(handle, 0.0, theme.accent.gamma_multiply(0.5));
            }
            if hr.dragged() {
                let d = hr.drag_delta().x;
                let avail = (cols.right - cols.name_left).max(MIN_NAME_W);
                if which == 0 {
                    let max = (avail - col_drag.1 - need.modified).max(MIN_NAME_W);
                    col_drag.0 = (cols.name_w + d).clamp(MIN_NAME_W, max);
                } else {
                    let max = (avail - col_drag.0 - need.modified).max(need.size);
                    col_drag.1 = (cols.size_w + d).clamp(need.size, max);
                }
            }
        }
    }
    if let Some(key) = resort {
        let spec = &mut tab.listing.sort;
        if spec.key == key {
            spec.ascending = !spec.ascending;
        } else {
            spec.key = key;
            spec.ascending = true;
        }
        tab.listing.apply_sort();
        interacted = true;
    }
    ui.separator();

    match tab.listing.state {
        ListingState::Error => {
            ui.colored_label(
                theme.error,
                tab.listing.error.clone().unwrap_or_else(|| "Error".into()),
            );
            return interacted;
        }
        ListingState::Loading if tab.listing.entries.is_empty() => {
            ui.weak("Loading…");
            return interacted;
        }
        _ => {}
    }

    // Visible = filtered view; indices map into listing.entries.
    let filter = tab.typeahead.to_lowercase();
    let visible: Vec<usize> = tab
        .listing
        .entries
        .iter()
        .enumerate()
        .filter(|(_, e)| entry_visible(e, show_hidden, &filter, view.chooser))
        .map(|(i, _)| i)
        .collect();

    let mut open_dir: Option<PathBuf> = None;
    let mut open_file: Option<PathBuf> = None;
    let mut new_tab: Option<PathBuf> = None;

    // Arrow-key browsing; anchor position in the visible order is "where we are".
    // Launched pointing at a file: select and reveal it the first frame it is actually in
    // the list. Name-keyed, so it survives sorting and the streaming order of the listing.
    if let Some(name) = tab.pending_reveal.clone() {
        if let Some(vi) = visible
            .iter()
            .position(|&ei| tab.listing.entries.get(ei).is_some_and(|e| e.name == name))
        {
            tab.selection.clear();
            tab.selection.insert(name.clone());
            tab.select_anchor = Some(name);
            tab.scroll_to = Some(vi);
            tab.pending_reveal = None;
        }
    }
    mark_first(tab, &visible, view);
    let folder_menu_key = is_active && menu_key(ui, tab, &visible, view);
    let current_vi = keyboard_position(tab, &visible);
    let page = (ui.available_height() / row_h).floor().max(1.0) as usize;
    if let Some((vi, shift)) = keyboard_nav(ui, is_active && !view.modal_open, visible.len(), current_vi, 1, page) {
        if shift {
            select_range_to(tab, &visible, vi);
        } else {
            select_index(tab, &visible, vi);
        }
        interacted = true;
    }
    // In the list Left and Right walk the tree, as in yazi: Right into the marked folder, Left
    // up to the parent with the folder left marked. A file is never opened by an arrow, only by
    // Enter. (The grid's arrows move the selection sideways instead.)
    if is_active && !ui.ctx().text_edit_focused() && !view.modal_open {
        let (left, right) = ui.input(|i| (i.modifiers.is_none() && i.key_pressed(egui::Key::ArrowLeft), i.modifiers.is_none() && i.key_pressed(egui::Key::ArrowRight)));
        if right {
            if let Some(entry) = current_vi.and_then(|vi| visible.get(vi)).and_then(|&ei| tab.listing.entries.get(ei)).filter(|e| e.is_dir) {
                open_dir = Some(entry.path.clone());
            }
        } else if left {
            tab.up();
            interacted = true;
        }
    }
    if is_active {
        interacted |= handle_shortcuts(ui, tab, view, &visible);
    }

    // The background is registered BEFORE the rows, so rows sit above it in the hit-test and
    // right-clicking an entry never reaches it. Its menu is shown after the rows instead,
    // because a row can ask for it too (see `folder_menu_from_row`).
    let bg = ui.interact(
        ui.available_rect_before_wrap(),
        ui.id().with(("list-bg", pane_idx, tab_idx)),
        // Drag as well as click: empty space is where rubber-band selection starts.
        egui::Sense::click_and_drag(),
    );
    // Set by a right-click in a row's Size or Modified column, which opens the folder menu.
    let mut folder_menu_from_row = false;
    // A drag is over a row's Size or Modified column: the drop goes into this folder, which is
    // outlined to say so.
    let mut drop_here = false;

    let ppp = ui.ctx().pixels_per_point();
    let mut area = egui::ScrollArea::vertical()
        // Without a salt every pane shares one scroll state, so they all scrolled together.
        .id_salt(("list", pane_idx, tab_idx))
        .auto_shrink([false, false]);
    // Reveal the keyboard selection: virtualized rows cannot use scroll_to_rect, so the
    // offset is computed from the row index instead.
    if let Some(vi) = tab.scroll_to.take() {
        let viewport_h = ui.available_height();
        // Clamp against BOTH ends. egui clamps a stored offset only at frame end, so an
        // out-of-range offset renders for one frame first, and the virtualized rows are
        // chosen from it, so the whole list appeared scrolled down for that frame and
        // snapped back the next. With arrow keys held, that alternated every repeat: a
        // visible flicker whenever the selection sat below the viewport's midpoint (for a
        // list that fits on screen, the max is 0 and ANY centring target overshoots).
        // show_rows places a row every row_h PLUS the item spacing; counting row_h alone put
        // the target a little higher with every row and capped the scroll short of the end,
        // so near the bottom of a long folder the selection ran out of view.
        let gap = ui.spacing().item_spacing.y;
        let stride = row_h + gap;
        let max_scroll = (visible.len() as f32 * stride - gap - viewport_h).max(0.0);
        let target =
            (vi as f32 * stride - viewport_h * 0.5 + row_h * 0.5).clamp(0.0, max_scroll);
        area = area.vertical_scroll_offset(target);
    }
    // The marking glides to its new row instead of jumping there (`glide_row`).
    let glide_at = glide_entry(ui.ctx(), pane_idx, tab_idx, tab, &visible, 1, settings.animations);
    let scroll_out = area.show_rows(ui, row_h, visible.len(), |ui, range| {
            // Held for the gliding marking: painted under the rows once their places are known.
            let glide_slot = ui.painter().add(egui::Shape::Noop);
            let mut glide_rows = None;
            // A row senses DRAG only across its Name column. Beyond that, the Size and
            // Modified columns, it senses click alone, so egui hands the drag to the
            // background instead and a rubber band can start there. Without this a folder
            // filled top to bottom offers no empty space at all, and every band attempt
            // turns into a drag-and-drop of whatever row was under the cursor.
            //
            // Every row shares the same x-extent, so the boundary is computed once here
            // rather than per row.
            let split_x = Cols::new(ui.available_rect_before_wrap(), pane_col_name, pane_col_size, need).size_left;
            // While the button is down, judge by where the press STARTED. Using the live
            // hover position would flip the sense mid-drag as the cursor crossed into the
            // Size column, cancelling a drag-and-drop halfway through.
            let probe_x = ui
                .input(|i| i.pointer.press_origin().or(i.pointer.hover_pos()))
                .map(|p| p.x);
            let name_area = probe_x.is_some_and(|x| x < split_x);

            for vi in range {
                let ei = visible[vi];
                // A menu action or rename commit earlier THIS frame may have reloaded the
                // tab, emptying `entries` while `visible` still holds stale indices, and
                // indexing them would panic (reproduced: New Folder crashed the app here).
                let Some(entry) = tab.listing.entries.get(ei) else { continue };
                let selected = tab.selection.contains(&entry.name);

                let (rect, resp) = ui.allocate_exact_size(
                    egui::vec2(ui.available_width(), row_h),
                    if name_area {
                        egui::Sense::click_and_drag()
                    } else {
                        egui::Sense::click()
                    },
                );
                // contains_pointer, NOT hovered: egui documents that a widget being dragged
                // stays the only hovered one even after the pointer leaves it, so during a
                // drag the highlight stuck to the row you grabbed and the folder under the
                // cursor never lit up. contains_pointer is what egui itself recommends for
                // drag-and-drop targets. The drop below reads this same binding, so what is
                // highlighted and what receives the drop cannot disagree.
                let hovered = resp.contains_pointer();
                let gliding = glide_at.is_some_and(|(_, to)| to == vi);
                if glide_at.is_some() && glide_rows.is_none() {
                    let stride = row_h + ui.spacing().item_spacing.y;
                    glide_rows = Some((rect.x_range(), rect.min.y - vi as f32 * stride, stride));
                }
                let bg = match (selected && !gliding, hovered) {
                    (true, true) => Some(theme.accent.gamma_multiply(0.42)),
                    (true, false) => Some(theme.accent.gamma_multiply(0.30)),
                    (false, true) => Some(theme.accent.gamma_multiply(0.18)),
                    (false, false) => None,
                };
                if let Some(bg) = bg {
                    ui.painter().rect_filled(rect, 2.0, bg);
                }

                // icon + name | size | modified, same Cols geometry as the header
                let cols = Cols::new(rect, pane_col_name, pane_col_size, need);
                let icon_rect = egui::Rect::from_min_size(
                    egui::pos2(cols.name_left, rect.min.y + (row_h - icon_sz) / 2.0),
                    egui::vec2(icon_sz, icon_sz),
                );
                // Cut, waiting to be pasted: faded, as other file managers show it.
                let ghost = if view.cut.contains(&entry.path) { CUT_ALPHA } else { 1.0 };
                if let Some(tex) = icons.get_for_entry(
                    ui.ctx(),
                    &entry.path,
                    entry.is_dir,
                    icon_sz,
                    ppp,
                    theme.icon_tint,
                ) {
                    ui.painter().image(
                        tex,
                        icon_rect,
                        egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                        egui::Color32::WHITE.gamma_multiply(ghost),
                    );
                }

                let text_col = if entry.hidden { theme.text.gamma_multiply(0.6) } else { theme.text }.gamma_multiply(ghost);
                let font = egui::TextStyle::Body.resolve(ui.style());
                let name_left = icon_rect.max.x + 6.0;
                // Where the name is drawn: clicking it (and nothing else in the row) renames.
                let name_text_rect = ui.painter().text(
                    egui::pos2(name_left, rect.center().y),
                    egui::Align2::LEFT_CENTER,
                    crate::text::elide_end(ui, &entry.name, cols.size_left - 6.0 - name_left),
                    font.clone(),
                    text_col,
                );
                if !entry.is_dir && cols.show_size {
                    ui.painter().text(
                        egui::pos2(cols.modified_left - 6.0, rect.center().y),
                        egui::Align2::RIGHT_CENTER,
                        crate::text::elide_start(ui, &human_size(entry.size), cols.size_w - 8.0),
                        font.clone(),
                        text_col.gamma_multiply(0.8),
                    );
                }
                if cols.show_modified {
                    ui.painter().text(
                        egui::pos2(cols.modified_left + 2.0, rect.center().y),
                        egui::Align2::LEFT_CENTER,
                        crate::text::elide_end(
                            ui,
                            &format_mtime(entry.modified, &settings.date_format),
                            cols.modified_w - 4.0,
                        ),
                        font,
                        text_col.gamma_multiply(0.6),
                    );
                }

                let entry_name = entry.name.clone();
                let entry_is_dir = entry.is_dir;
                let entry_path = entry.path.clone();

                // Inline rename overlays the name area of its row.
                if matches!(&tab.renaming, Some((orig, _)) if *orig == entry_name) {
                    let edit_rect = egui::Rect::from_min_max(
                        egui::pos2(icon_rect.max.x + 4.0, rect.min.y + 1.0),
                        egui::pos2(cols.size_left - 6.0, rect.max.y - 1.0),
                    );
                    rename_editor(ui, tab, edit_rect, view);
                }

                // --- drag and drop ---
                if view.chooser.is_none() && (resp.drag_started_by(egui::PointerButton::Primary) || resp.drag_started_by(egui::PointerButton::Secondary)) {
                    let mut paths = Vec::new();
                    if !tab.selection.contains(&entry_name) {
                        tab.selection.clear();
                        tab.selection.insert(entry_name.clone());
                        tab.select_anchor = Some(entry_name.clone());
                    }
                    for name in &tab.selection {
                        if let Some(e) = tab.listing.entries.iter().find(|e| &e.name == name) {
                            paths.push(e.path.clone());
                        }
                    }
                    let secondary = resp.drag_started_by(egui::PointerButton::Secondary);
                    ui.ctx().memory_mut(|m| m.data.insert_temp(egui::Id::new("dnd"), DragPayload { source_paths: paths, secondary }));
                }

                // A drop on a folder's NAME goes into it; in the Size and Modified columns it
                // goes into the folder this pane shows, the same split as for right-clicks. A
                // list full of folders would otherwise leave nowhere to drop "here".
                if dragging(ui) && hovered {
                    let on_name = ui.input(|i| i.pointer.hover_pos()).is_some_and(|p| p.x < split_x);
                    let into_row = entry_is_dir && on_name;
                    // On a file's name nothing happens, as before.
                    let into_here = !on_name;
                    if into_row {
                        ui.painter().rect_filled(rect, 2.0, theme.accent.gamma_multiply(0.5));
                        mark_drop_target(ui, &entry_path);
                    } else if into_here {
                        drop_here = true;
                    }
                    if ui.input(|i| i.pointer.any_released()) {
                        if into_row {
                            drop_into(ui, view, &entry_path);
                        } else if into_here {
                            drop_into(ui, view, tab.path());
                        }
                    }
                }

                // --- interactions ---
                // A right-click in the Size or Modified column opens the FOLDER menu. In a
                // folder filled top to bottom the only empty space left is the thin gap between
                // rows, too thin to hit, so New Folder, New File and Paste were close to
                // unreachable. The Name column is the file; the columns to its right belong to
                // the folder, the same split that already hands a drag there to the background.
                let on_name = resp.interact_pointer_pos().is_some_and(|p| p.x < split_x);
                if resp.secondary_clicked() {
                    interacted = true;
                    if on_name {
                        fix_selection_for_context(tab, &entry_name);
                    } else {
                        // Selection is left alone, as it is for a right-click on real empty
                        // space: the folder menu does not act on it.
                        folder_menu_from_row = true;
                    }
                }
                // Response::context_menu would open on ANY secondary click, so the open decision
                // is spelled out: the same rules egui uses, but only for the Name column.
                let entry_menu = if resp.secondary_clicked() {
                    Some(egui::SetOpenCommand::Bool(on_name))
                } else if resp.clicked() {
                    Some(egui::SetOpenCommand::Bool(false))
                } else {
                    None
                };
                let key_at = (tab.menu_for.as_deref() == Some(entry_name.as_str())).then(|| {
                    tab.menu_for = None;
                    egui::pos2(resp.rect.left() + 28.0, resp.rect.bottom())
                });
                context_popup(&resp, entry_menu, key_at, |ui| entry_context_menu(ui, tab, &entry_name, entry_is_dir, view));

                if resp.double_clicked() {
                    // Cancels the rename the first press of this double-click armed.
                    tab.rename_armed = None;
                    if entry_is_dir {
                        open_dir = Some(entry_path.clone());
                    } else {
                        open_file = Some(entry_path.clone());
                    }
                } else if resp.middle_clicked() && entry_is_dir && view.chooser.is_none() {
                    new_tab = Some(entry_path.clone());
                } else if resp.clicked() {
                    interacted = true;
                    let mods = ui.input(|i| i.modifiers);
                    // Clicking the NAME of a file that is already the only selection starts
                    // an inline rename, as in Dolphin. Only the name's own text counts: the
                    // icon, the space after the name and the other columns re-select without
                    // renaming.
                    let rearm = view.chooser.is_none()
                        && !mods.ctrl
                        && !mods.shift
                        && tab.renaming.is_none()
                        && tab.selection.len() == 1
                        && tab.selection.contains(&entry_name)
                        && resp.interact_pointer_pos().is_some_and(|p| name_text_rect.expand(2.0).contains(p));
                    tab.rename_armed =
                        rearm.then(|| (entry_name.clone(), std::time::Instant::now()));
                    if mods.ctrl {
                        if !tab.selection.remove(&entry_name) {
                            tab.selection.insert(entry_name.clone());
                        }
                        tab.select_anchor = Some(entry_name.clone());
                    } else if mods.shift {
                        // Range from the anchor to here within the visible order.
                        select_range_to(tab, &visible, vi);
                    } else {
                        tab.selection.clear();
                        tab.selection.insert(entry_name.clone());
                        tab.select_anchor = Some(entry_name.clone());
                    }
                }
            }
            if let (Some((at, _)), Some((x, y0, stride))) = (glide_at, glide_rows) {
                let y = y0 + at.y * stride;
                let r = egui::Rect::from_x_y_ranges(x, y..=y + row_h);
                ui.painter().set(glide_slot, egui::Shape::rect_filled(r, 2.0, theme.accent.gamma_multiply(0.30)));
            }
        });

    interacted |= fire_armed_rename(ui, tab);

    // Opens at the pointer for a right-click on real empty space OR in a row's Size or
    // Modified column. Only one popup is open per viewport and keeping one open never reopens it, so
    // showing this after the rows cannot steal a file menu a row just opened.
    let folder_menu = if bg.secondary_clicked() || folder_menu_from_row {
        Some(egui::SetOpenCommand::Bool(true))
    } else if bg.clicked() {
        Some(egui::SetOpenCommand::Bool(false))
    } else {
        None
    };
    let key_at = folder_menu_key.then(|| scroll_out.inner_rect.left_top() + egui::vec2(28.0, 28.0));
    context_popup(&bg, folder_menu, key_at, |ui| folder_context_menu(ui, tab, view));

    {
        let viewport = scroll_out.inner_rect;
        let scroll_y = scroll_out.state.offset.y;
        let content_top = viewport.min.y - scroll_y;
        let n = visible.len();
        interacted |= rubber_band(
            ui,
            &bg,
            &BandView { viewport, scroll_y, accent: theme.accent },
            tab,
            &visible,
            |band, out| {
                if row_h <= 0.0 {
                    return;
                }
                let lo = ((band.min.y - content_top) / row_h).floor().max(0.0) as usize;
                let hi = ((band.max.y - content_top) / row_h).ceil().max(0.0) as usize;
                for vi in lo..hi.min(n) {
                    out(vi);
                }
            },
        );

        // Clicking empty space deselects. ctrl/shift are the "I am adding to a selection"
        // modifiers everywhere else here, so they hold it, otherwise a mis-aimed ctrl+click
        // beside a row would throw away the selection it was meant to extend.
        if bg.clicked()
            && !ui.input(|i| i.modifiers.ctrl || i.modifiers.shift)
            && !tab.selection.is_empty()
        {
            tab.selection.clear();
            tab.select_anchor = None;
            interacted = true;
        }
    }

    // The folder shown here is the target: outlined, so it is never a guess whether the drop
    // goes into it or into the folder under the pointer.
    if drop_here || (dragging(ui) && bg.hovered()) {
        mark_drop_target(ui, tab.path());
        ui.painter().rect_stroke(bg.rect.shrink(1.0), 3.0, egui::Stroke::new(2.0, theme.accent), egui::StrokeKind::Inside);
    }
    if bg.hovered() {
        if ui.input(|i| i.pointer.any_released()) {
            drop_into(ui, view, tab.path());
        }
        
        let dropped = if view.chooser.is_some() { Vec::new() } else { ui.input(|i| i.raw.dropped_files.clone()) };
        if !dropped.is_empty() {
            let mut paths = Vec::new();
            for f in dropped {
                paths.push(f.path().to_path_buf());
            }
            if !paths.is_empty() {
                let copy = ui.input(|i| i.modifiers.ctrl);
                view.actions.borrow_mut().push(Action::Drop { sources: paths, dest: tab.path().to_path_buf(), copy });
            }
        }
    }

    if let Some(p) = open_dir {
        tab.navigate(p);
        interacted = true;
    }
    if let Some(p) = open_file {
        view.actions.borrow_mut().push(Action::Activate(vec![p]));
        interacted = true;
    }
    if let Some(p) = new_tab {
        pane.tabs.push(crate::panes::Tab::new(p));
        pane.active_tab = pane.tabs.len() - 1;
        interacted = true;
    }
    if col_drag != (pane_col_name, pane_col_size) {
        pane.col_name_w = col_drag.0;
        pane.col_size_w = col_drag.1;
        interacted = true;
    }
    interacted
}

/// Icon grid. Cell width follows the zoom; the label sits under the thumbnail.
/// Logical icon size at which folders switch from their icon to an image collage. Below
/// this, four tiles in one cell are too small to tell apart.
const FOLDER_PREVIEW_MIN_PX: f32 = 64.0;

/// Turn an armed click into an inline rename once the double-click window has passed.
///
/// Shared by both views: renaming by clicking the name of an already-selected item is not a
/// list-view feature, it is how renaming works.
fn fire_armed_rename(ui: &egui::Ui, tab: &mut crate::panes::Tab) -> bool {
    let Some((name, armed_at)) = tab.rename_armed.clone() else { return false };
    let waited = armed_at.elapsed();
    // The delay must exceed egui's double-click time (0.3s), or opening a file would rename
    // it on the way.
    const ARM_DELAY: std::time::Duration = std::time::Duration::from_millis(400);
    if waited < ARM_DELAY {
        // Nothing else will wake us in a quiet window, ask for the frame explicitly.
        ui.ctx().request_repaint_after(ARM_DELAY - waited);
        return false;
    }
    tab.rename_armed = None;
    // Re-check: the selection may have moved on while we waited.
    if tab.renaming.is_some() || tab.selection.len() != 1 || !tab.selection.contains(&name) {
        return false;
    }
    tab.renaming = Some((name.clone(), name));
    // The rows, and with them the rename editor, were drawn earlier THIS frame, so the
    // field first appears in the next one. Ask for it: otherwise the loop goes back to sleep
    // and the edit box stays invisible until some unrelated event happens to wake it, which
    // looked like "renaming only shows up once I move the mouse".
    ui.ctx().request_repaint();
    true
}

fn show_grid(
    ui: &mut egui::Ui,
    pane: &mut Pane,
    pane_idx: usize,
    is_active: bool,
    icons: &mut IconCache,
    view: &View,
) -> bool {
    let theme = view.theme;
    let show_hidden = pane.show_hidden;
    let zoom = pane.zoom;
    let tab_idx = pane.active_tab;
    let tab = pane.tab();
    let mut interacted = false;

    if tab.listing.state == ListingState::Error {
        ui.colored_label(
            theme.error,
            tab.listing.error.clone().unwrap_or_else(|| "Error".into()),
        );
        return interacted;
    }

    let filter = tab.typeahead.to_lowercase();
    let visible: Vec<usize> = tab
        .listing
        .entries
        .iter()
        .enumerate()
        .filter(|(_, e)| entry_visible(e, show_hidden, &filter, view.chooser))
        .map(|(i, _)| i)
        .collect();

    let pad = 10.0;
    let label_h = ui.text_style_height(&egui::TextStyle::Body) + 4.0;
    let cell_w = zoom + pad * 2.0;
    let cell_h = zoom + label_h + pad;
    let cols = ((ui.available_width() / cell_w).floor() as usize).max(1);
    let rows = visible.len().div_ceil(cols);

    let mut open_dir = None;
    let mut open_file = None;
    let ppp = ui.ctx().pixels_per_point();
    let uv = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));

    // Launched pointing at a file: select and reveal it the first frame it is actually in
    // the list. Name-keyed, so it survives sorting and the streaming order of the listing.
    if let Some(name) = tab.pending_reveal.clone() {
        if let Some(vi) = visible
            .iter()
            .position(|&ei| tab.listing.entries.get(ei).is_some_and(|e| e.name == name))
        {
            tab.selection.clear();
            tab.selection.insert(name.clone());
            tab.select_anchor = Some(name);
            tab.scroll_to = Some(vi);
            tab.pending_reveal = None;
        }
    }
    mark_first(tab, &visible, view);
    let folder_menu_key = is_active && menu_key(ui, tab, &visible, view);
    let current_vi = keyboard_position(tab, &visible);
    let page = (ui.available_height() / cell_h).floor().max(1.0) as usize;
    if let Some((vi, shift)) = keyboard_nav(ui, is_active && !view.modal_open, visible.len(), current_vi, cols, page) {
        if shift {
            select_range_to(tab, &visible, vi);
        } else {
            select_index(tab, &visible, vi);
        }
        interacted = true;
    }
    if is_active {
        interacted |= handle_shortcuts(ui, tab, view, &visible);
    }
    let bg = ui.interact(
        ui.available_rect_before_wrap(),
        ui.id().with(("grid-bg", pane_idx, tab_idx)),
        // Drag as well as click: empty space is where rubber-band selection starts.
        egui::Sense::click_and_drag(),
    );
    let key_at = folder_menu_key.then(|| bg.rect.left_top() + egui::vec2(28.0, 28.0));
    context_popup(&bg, right_click_opens(&bg), key_at, |ui| folder_context_menu(ui, tab, view));

    let mut area = egui::ScrollArea::vertical()
        .id_salt(("grid", pane_idx, tab_idx))
        .auto_shrink([false, false]);
    if let Some(vi) = tab.scroll_to.take() {
        let viewport_h = ui.available_height();
        let row = (vi / cols) as f32;
        // Clamped against both ends, see the list view for the one-frame-flicker this
        // prevents.
        // Rows sit cell_h plus the item spacing apart, as in the list view.
        let gap = ui.spacing().item_spacing.y;
        let stride = cell_h + gap;
        let max_scroll = (rows as f32 * stride - gap - viewport_h).max(0.0);
        let target =
            (row * stride - viewport_h * 0.5 + cell_h * 0.5).clamp(0.0, max_scroll);
        area = area.vertical_scroll_offset(target);
    }
    // The marking glides to its new cell, as in the list.
    let glide_at = glide_entry(ui.ctx(), pane_idx, tab_idx, tab, &visible, cols, view.settings.animations);
    let scroll_out = area.show_rows(ui, cell_h, rows, |ui, range| {
            let glide_slot = ui.painter().add(egui::Shape::Noop);
            // Where cell (0, 0) would stand, and how far apart cells are.
            let mut glide_cells: Option<(egui::Pos2, egui::Vec2)> = None;
            let first_row = range.start;
            for row in range {
                ui.horizontal(|ui| {
                    for col in 0..cols {
                        let Some(&ei) = visible.get(row * cols + col) else { break };
                        // Same stale-index guard as the list view (see there).
                        let Some(entry) = tab.listing.entries.get(ei) else { break };
                        let selected = tab.selection.contains(&entry.name);

                        let (rect, resp) = ui
                            .allocate_exact_size(egui::vec2(cell_w, cell_h), egui::Sense::click_and_drag());
                        let gliding = glide_at.is_some_and(|(_, to)| to == row * cols + col);
                        if glide_at.is_some() && glide_cells.is_none() && row == first_row && col == 0 {
                            let gap = ui.spacing().item_spacing;
                            let stride = egui::vec2(cell_w + gap.x, cell_h + gap.y);
                            glide_cells = Some((rect.min - egui::vec2(0.0, row as f32 * stride.y), stride));
                        }
                        let bg = match (selected && !gliding, resp.hovered()) {
                            (true, true) => Some(theme.accent.gamma_multiply(0.42)),
                            (true, false) => Some(theme.accent.gamma_multiply(0.30)),
                            (false, true) => Some(theme.accent.gamma_multiply(0.18)),
                            (false, false) => None,
                        };
                        if let Some(bg) = bg {
                            ui.painter().rect_filled(rect, 3.0, bg);
                        }

                        let img_rect = egui::Rect::from_center_size(
                            egui::pos2(rect.center().x, rect.min.y + pad + zoom * 0.5),
                            egui::vec2(zoom, zoom),
                        );

                        // Cut, waiting to be pasted: faded, as other file managers show it.
                        let tint = egui::Color32::WHITE.gamma_multiply(if view.cut.contains(&entry.path) { CUT_ALPHA } else { 1.0 });
                        // A real thumbnail when we can make one, otherwise the MIME icon.
                        let mut drew_thumb = false;
                        // Folder preview: a collage of up to four images from inside the
                        // folder, once the cell is big enough for the tiles to read as
                        // anything. Below that the plain folder icon is clearer.
                        if entry.is_dir && zoom >= FOLDER_PREVIEW_MIN_PX {
                            let samples = view.thumbs.borrow_mut().folder_sample(&entry.path);
                            let mut thumbs = view.thumbs.borrow_mut();
                            let mut tiles = Vec::new();
                            for (path, mtime) in &samples {
                                let key = crate::thumbs::Thumbs::key(path, *mtime);
                                thumbs.request(&key, path);
                                if let Some(crate::thumbs::Slot::Ready(tex)) = thumbs.get(&key) {
                                    tiles.push(tex.clone());
                                }
                            }
                            if !tiles.is_empty() {
                                // Dolphin's arrangement: the folder icon still reads as a
                                // folder, with the photos laid ON its face. A bare collage
                                // loses the "this is a folder" cue entirely.
                                if let Some(tex) = icons.get_for_entry(
                                    ui.ctx(),
                                    &entry.path,
                                    true,
                                    zoom,
                                    ppp,
                                    theme.icon_tint,
                                ) {
                                    ui.painter().image(tex, img_rect, uv, tint);
                                }

                                // Keep the tiles well inside the folder body. The icon's own
                                // artwork does not fill `img_rect` edge to edge and its margins
                                // differ per theme and size bucket, so anchoring to the rect's
                                // edges put tiles outside the drawn folder. A small centred
                                // patch, nudged down clear of the tab, is safe for any art.
                                let (w, h) = (img_rect.width(), img_rect.height());
                                let face = egui::Rect::from_center_size(
                                    egui::pos2(img_rect.center().x, img_rect.center().y + h * 0.07),
                                    egui::vec2(w * 0.58, h * 0.46),
                                );
                                // Fixed 2x2 regardless of how many are ready, so tiles do not
                                // jump around as the rest finish decoding.
                                let half = face.size() * 0.5;
                                let border = (zoom * 0.015).max(1.0);
                                for (i, tex) in tiles.iter().enumerate() {
                                    let cell = egui::Rect::from_min_size(
                                        face.min
                                            + egui::vec2(
                                                (i % 2) as f32 * half.x,
                                                (i / 2) as f32 * half.y,
                                            ),
                                        half,
                                    )
                                    .shrink(border);
                                    let sz = tex.size_vec2();
                                    let fitted =
                                        crate::preview::fit(cell, sz.x as u32, sz.y as u32);
                                    // White mount under each photo, as Dolphin draws them.
                                    ui.painter().rect_filled(
                                        fitted.expand(border),
                                        0.0,
                                        tint,
                                    );
                                    ui.painter().image(
                                        tex.id(),
                                        fitted,
                                        uv,
                                        tint,
                                    );
                                }
                                drew_thumb = true;
                            }
                        }
                        if !entry.is_dir && crate::thumbs::thumbable(&entry.path) {
                            let key = crate::thumbs::Thumbs::key(&entry.path, entry.modified);
                            let mut thumbs = view.thumbs.borrow_mut();
                            thumbs.request(&key, &entry.path);
                            if let Some(crate::thumbs::Slot::Ready(tex)) = thumbs.get(&key) {
                                let size = tex.size_vec2();
                                let fitted =
                                    crate::preview::fit(img_rect, size.x as u32, size.y as u32);
                                ui.painter().image(tex.id(), fitted, uv, tint);
                                drew_thumb = true;
                            }
                        }
                        if !drew_thumb {
                            if let Some(tex) = icons.get_for_entry(
                                ui.ctx(),
                                &entry.path,
                                entry.is_dir,
                                zoom,
                                ppp,
                                theme.icon_tint,
                            ) {
                                ui.painter().image(tex, img_rect, uv, tint);
                            }
                        }

                        let text_col = if entry.hidden {
                            theme.text.gamma_multiply(0.6)
                        } else {
                            theme.text
                        }
                        .gamma_multiply(tint.a() as f32 / 255.0);
                        let name_text_rect = ui.painter().text(
                            egui::pos2(rect.center().x, rect.max.y - label_h * 0.5),
                            egui::Align2::CENTER_CENTER,
                            crate::text::elide_end(ui, &entry.name, cell_w - 6.0),
                            egui::TextStyle::Body.resolve(ui.style()),
                            text_col,
                        );

                        let entry_name = entry.name.clone();
                        let entry_is_dir = entry.is_dir;
                        let entry_path = entry.path.clone();

                        if matches!(&tab.renaming, Some((orig, _)) if *orig == entry_name) {
                            let edit_rect = egui::Rect::from_min_max(
                                egui::pos2(rect.min.x + 2.0, rect.max.y - label_h),
                                egui::pos2(rect.max.x - 2.0, rect.max.y - 2.0),
                            );
                            rename_editor(ui, tab, edit_rect, view);
                        }
                        // --- drag and drop ---
                        if view.chooser.is_none() && (resp.drag_started_by(egui::PointerButton::Primary) || resp.drag_started_by(egui::PointerButton::Secondary)) {
                            let mut paths = Vec::new();
                            if !tab.selection.contains(&entry_name) {
                                tab.selection.clear();
                                tab.selection.insert(entry_name.clone());
                                tab.select_anchor = Some(entry_name.clone());
                            }
                            for name in &tab.selection {
                                if let Some(e) = tab.listing.entries.iter().find(|e| &e.name == name) {
                                    paths.push(e.path.clone());
                                }
                            }
                            let secondary = resp.drag_started_by(egui::PointerButton::Secondary);
                    ui.ctx().memory_mut(|m| m.data.insert_temp(egui::Id::new("dnd"), DragPayload { source_paths: paths, secondary }));
                        }

                        let hovered = resp.contains_pointer(); // see the list view for why
                        if dragging(ui) && hovered && entry_is_dir {
                            ui.painter().rect_filled(rect, 3.0, theme.accent.gamma_multiply(0.5));
                            mark_drop_target(ui, &entry_path);
                            if ui.input(|i| i.pointer.any_released()) {
                                drop_into(ui, view, &entry_path);
                            }
                        }

                        if resp.secondary_clicked() {
                            fix_selection_for_context(tab, &entry_name);
                            interacted = true;
                        }
                        let key_at = (tab.menu_for.as_deref() == Some(entry_name.as_str())).then(|| {
                            tab.menu_for = None;
                            resp.rect.center()
                        });
                        context_popup(&resp, right_click_opens(&resp), key_at, |ui| entry_context_menu(ui, tab, &entry_name, entry_is_dir, view));

                        if resp.double_clicked() {
                            // Cancels the rename the first press of this double-click armed.
                            tab.rename_armed = None;
                            if entry_is_dir {
                                open_dir = Some(entry_path.clone());
                            } else {
                                open_file = Some(entry_path.clone());
                            }
                        } else if resp.clicked() {
                            interacted = true;
                            let mods = ui.input(|i| i.modifiers);
                            // Clicking the NAME of the single selected item starts an inline
                            // rename, exactly as in the list view: only the label's own text
                            // counts, not the icon above it or the band beside a short name.
                            let rearm = view.chooser.is_none()
                                && !mods.ctrl
                                && !mods.shift
                                && tab.renaming.is_none()
                                && tab.selection.len() == 1
                                && tab.selection.contains(&entry_name)
                                && resp
                                    .interact_pointer_pos()
                                    .is_some_and(|p| name_text_rect.expand(2.0).contains(p));
                            tab.rename_armed =
                                rearm.then(|| (entry_name.clone(), std::time::Instant::now()));
                            if mods.ctrl {
                                if !tab.selection.remove(&entry_name) {
                                    tab.selection.insert(entry_name.clone());
                                }
                                tab.select_anchor = Some(entry_name.clone());
                            } else if mods.shift {
                                // Range from the anchor to here, as in the list view.
                                select_range_to(tab, &visible, row * cols + col);
                            } else {
                                tab.selection.clear();
                                tab.selection.insert(entry_name.clone());
                                tab.select_anchor = Some(entry_name.clone());
                            }
                        }
                    }
                });
            }
            if let (Some((at, _)), Some((origin, stride))) = (glide_at, glide_cells) {
                let r = egui::Rect::from_min_size(origin + at * stride, egui::vec2(cell_w, cell_h));
                ui.painter().set(glide_slot, egui::Shape::rect_filled(r, 3.0, theme.accent.gamma_multiply(0.30)));
            }
        });

    interacted |= fire_armed_rename(ui, tab);

    {
        let viewport = scroll_out.inner_rect;
        let scroll_y = scroll_out.state.offset.y;
        let content_top = viewport.min.y - scroll_y;
        let content_left = viewport.min.x;
        let n = visible.len();
        interacted |= rubber_band(
            ui,
            &bg,
            &BandView { viewport, scroll_y, accent: theme.accent },
            tab,
            &visible,
            |band, out| {
                if cell_h <= 0.0 || cell_w <= 0.0 || cols == 0 {
                    return;
                }
                let r0 = ((band.min.y - content_top) / cell_h).floor().max(0.0) as usize;
                let r1 = ((band.max.y - content_top) / cell_h).ceil().max(0.0) as usize;
                let c0 = ((band.min.x - content_left) / cell_w).floor().max(0.0) as usize;
                let c1 = ((band.max.x - content_left) / cell_w).ceil().max(0.0) as usize;
                for row in r0..r1.min(rows) {
                    for col in c0..c1.min(cols) {
                        let vi = row * cols + col;
                        if vi < n {
                            out(vi);
                        }
                    }
                }
            },
        );

        // Clicking empty space deselects. ctrl/shift are the "I am adding to a selection"
        // modifiers everywhere else here, so they hold it, otherwise a mis-aimed ctrl+click
        // beside a row would throw away the selection it was meant to extend.
        if bg.clicked()
            && !ui.input(|i| i.modifiers.ctrl || i.modifiers.shift)
            && !tab.selection.is_empty()
        {
            tab.selection.clear();
            tab.select_anchor = None;
            interacted = true;
        }
    }

    if dragging(ui) && bg.hovered() {
        mark_drop_target(ui, tab.path());
        ui.painter().rect_stroke(bg.rect.shrink(1.0), 3.0, egui::Stroke::new(2.0, theme.accent), egui::StrokeKind::Inside);
    }
    if bg.hovered() {
        if ui.input(|i| i.pointer.any_released()) {
            drop_into(ui, view, tab.path());
        }
        
        let dropped = if view.chooser.is_some() { Vec::new() } else { ui.input(|i| i.raw.dropped_files.clone()) };
        if !dropped.is_empty() {
            let mut paths = Vec::new();
            for f in dropped {
                paths.push(f.path().to_path_buf());
            }
            if !paths.is_empty() {
                let copy = ui.input(|i| i.modifiers.ctrl);
                view.actions.borrow_mut().push(Action::Drop { sources: paths, dest: tab.path().to_path_buf(), copy });
            }
        }
    }

    if let Some(p) = open_dir {
        tab.navigate(p);
        interacted = true;
    }
    if let Some(p) = open_file {
        view.actions.borrow_mut().push(Action::Activate(vec![p]));
        interacted = true;
    }
    interacted
}

/// The window-level preview panel. Shows the ACTIVE pane's selection.
pub fn show_preview(ui: &mut egui::Ui, pane: &Pane, view: &View) {
    let theme = view.theme;
    let settings = view.settings;
    if view.preview_focused {
        // Where the keys are, as the sidebar marks its entry.
        ui.painter().rect_stroke(ui.max_rect().shrink(1.0), 3.0, egui::Stroke::new(1.5, theme.accent), egui::StrokeKind::Inside);
    }
    ui.separator();
    ui.heading(egui::RichText::new("Preview").color(theme.accent_text).size(14.0));

    let tab = pane.tab_ref();
    let selected: Vec<&crate::model::Entry> = tab
        .listing
        .entries
        .iter()
        .filter(|e| tab.selection.contains(&e.name))
        .collect();

    match selected.as_slice() {
        [] => {
            ui.weak("Nothing selected");
        }
        [e] => {
            let editable = !e.is_dir && view.chooser.is_none() && crate::docs::is_text(&e.path) && !e.path.extension().is_some_and(|x| x.eq_ignore_ascii_case("svg"));
            let editing = view.editor.borrow().as_ref().is_some_and(|ed| ed.path == e.path);
            // One row, as high as a line: a right-to-left layout on its own would take all the
            // height there is and push the preview out of sight.
            let row_h = ui.text_style_height(&egui::TextStyle::Body) + 4.0;
            ui.allocate_ui_with_layout(egui::vec2(ui.available_width(), row_h), egui::Layout::left_to_right(egui::Align::Center), |ui| {
                if editable && !editing {
                    let why = crate::editor::refusal(&e.path);
                    let button = ui.add_enabled(why.is_none(), egui::Button::new("Edit").small());
                    let button = match why {
                        Some(w) => button.on_disabled_hover_text(w),
                        None => button.on_hover_text("Edit the text here (Ctrl+E)"),
                    };
                    if button.clicked() || view.edit_key {
                        match crate::editor::Editor::open(&e.path) {
                            Ok(ed) => *view.editor.borrow_mut() = Some(ed),
                            Err(why) => view.editor_event.set(Some(crate::editor::Event::Failed(why))),
                        }
                    }
                }
                ui.add(egui::Label::new(egui::RichText::new(&e.name).strong()).truncate());
            });
            if !e.is_dir {
                ui.label(human_size(e.size));
            } else {
                let roots = vec![e.path.clone()];
                if !view.dir_size.matches(&roots) {
                    view.actions.borrow_mut().push(Action::CalculateDirSize(roots.clone()));
                }
                let b = view.dir_size.bytes;
                let f = view.dir_size.files;
                if view.dir_size.matches(&roots) {
                    if view.dir_size.done {
                        ui.label(format!("{} ({} items)", human_size(b), f));
                    } else {
                        ui.label(format!("{} ({} items)...", human_size(b), f));
                    }
                }
            }
            ui.label(format_mtime(e.modified, &settings.date_format));
            if e.is_symlink {
                ui.weak("symlink");
            }
            ui.add_space(6.0);

            if let Some(ed) = view.editor.borrow_mut().as_mut().filter(|ed| ed.path == e.path) {
                // Still the preview's subject, so leaving the file is noticed.
                view.want_preview.set(Some((e.path.clone(), 0)));
                let asked = crate::editor::show(ui, ed, view.theme);
                if asked.save {
                    view.editor_event.set(Some(crate::editor::Event::Save));
                } else if asked.cancel {
                    view.editor_event.set(Some(crate::editor::Event::Cancel));
                }
                return;
            }
            if !e.is_dir && crate::preview::supported(&e.path) {
                let current_page = if view.preview.current_path() == Some(&e.path) {
                    view.preview.current_page().unwrap_or(0)
                } else {
                    0
                };
                view.want_preview.set(Some((e.path.clone(), current_page)));
                image_area(ui, view, &e.path);
            }
        }
        many => {
            ui.label(format!("{} items", many.len()));
            let (files, _, file_bytes) = crate::model::selection_totals(many.iter().copied());
            let roots: Vec<std::path::PathBuf> =
                many.iter().filter(|e| e.is_dir).map(|e| e.path.clone()).collect();
            if roots.is_empty() {
                ui.label(human_size(file_bytes));
            } else {
                // Folders are walked in the background; until that finishes the total is a
                // lower bound, marked with an ellipsis rather than shown as if it were final.
                // The status bar asks for the walk; this panel only reads the result, so a
                // single selection cannot start two.
                let (dir_bytes, settled) = if view.dir_size.matches(&roots) {
                    (view.dir_size.bytes, view.dir_size.done)
                } else {
                    (0, false)
                };
                ui.label(format!(
                    "{}{}",
                    human_size(file_bytes + dir_bytes),
                    if settled { "" } else { "..." }
                ));
                ui.weak(format!("{files} files + {} folders", roots.len()));
            }
        }
    }
}

/// Reserves space for the image and reports the fitted rect. Nothing is painted here on
/// purpose: the area must stay TRANSPARENT in the egui layer so the HDR pass, which drew
/// underneath, shows through the composite.
fn image_area(ui: &mut egui::Ui, view: &View, path: &std::path::Path) {
    use crate::preview::PreviewState;

    let mut avail = ui.available_size();
    if let PreviewState::Ready(p, _, total_pages, file_info) = &view.preview.state {
        if p == path {
            if *total_pages > 1 {
                avail.y -= 24.0;
            }
            if file_info.is_some() {
                avail.y -= 36.0;
            }
            avail.y -= 16.0; // padding
        }
    }
    
    // Text fills the pane; images/rasters keep a square-ish stage.
    let is_text = matches!(view.preview.shown, crate::preview::Shown::Text(_) | crate::preview::Shown::Rich(_));
    let height = if is_text { avail.y.max(48.0) } else { avail.x.min(avail.y).max(48.0) };
    let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), height), egui::Sense::hover());

    match &view.preview.state {
        PreviewState::Ready(p, current_page, total_pages, _) if p == path => match &view.preview.shown {
            crate::preview::Shown::Hdr => {
                if let Some((iw, ih)) = view.preview.texture_size {
                    let fitted = crate::preview::fit(rect, iw, ih);
                    view.hdr_rect.set(Some(fitted));
                    // A hairline frame so the image reads as a deliberate element even when
                    // it is mostly black, true black is indistinguishable from the
                    // background here.
                    ui.painter().rect_stroke(
                        fitted,
                        2.0,
                        egui::Stroke::new(1.0, view.theme.text.gamma_multiply(0.2)),
                        egui::StrokeKind::Outside,
                    );
                    if view.preview.is_hdr {
                        ui.painter().text(
                            egui::pos2(fitted.max.x - 4.0, fitted.min.y + 2.0),
                            egui::Align2::RIGHT_TOP,
                            "HDR",
                            egui::FontId::proportional(10.0),
                            view.theme.accent_text,
                        );
                    }
                }
            }
            crate::preview::Shown::Raster(tex) => {
                if let Some((iw, ih)) = view.preview.texture_size {
                    let fitted = crate::preview::fit(rect, iw, ih);
                    ui.painter().image(
                        tex.id(),
                        fitted,
                        egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                        egui::Color32::WHITE,
                    );
                    ui.painter().rect_stroke(
                        fitted,
                        2.0,
                        egui::Stroke::new(1.0, view.theme.text.gamma_multiply(0.2)),
                        egui::StrokeKind::Outside,
                    );
                }
            }
            crate::preview::Shown::Text(job) => {
                let mut child = ui.new_child(
                    egui::UiBuilder::new()
                        .max_rect(rect)
                        .layout(egui::Layout::top_down(egui::Align::Min)),
                );
                egui::ScrollArea::both()
                    .id_salt("preview-text")
                    .auto_shrink([false, false])
                    .show(&mut child, |ui| {
                        take_preview_scroll(ui);
                        ui.horizontal_top(|ui| {
                            line_numbers(ui, &job.text, view.theme);
                            ui.label(egui::text::LayoutJob::clone(job));
                        });
                    });
            }
            crate::preview::Shown::Rich(rich) => {
                let dark = view.settings.dark_pages.applies(false, view.theme.is_dark()) && !view.preview.show_original.get();
                crate::office::view::show(ui, rect, rich, view.theme, dark)
            }
            crate::preview::Shown::Nothing => {}
        },
        PreviewState::Failed(p, err) if p == path => {
            ui.painter().text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                crate::text::elide_end(ui, err, rect.width() - 8.0),
                egui::TextStyle::Body.resolve(ui.style()),
                view.theme.error,
            );
        }
        _ => {
            ui.painter().text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                "Decoding…",
                egui::TextStyle::Body.resolve(ui.style()),
                view.theme.text.gamma_multiply(0.6),
            );
        }
    }
    
    if let PreviewState::Ready(p, current_page, total_pages, file_info) = &view.preview.state {
        if p == path {
            // The controls follow the page that was ASKED for, not the one that happens to be
            // decoded: while a seek is in flight the state still describes the held frame, and
            // reading it here would drag the slider back out from under the pointer.
            let asked_page = view.preview.current_page().unwrap_or(*current_page);
            ui.add_space(4.0);
            ui.horizontal_wrapped(|ui| {
                if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
                    ui.weak(ext.to_uppercase());
                }
                if let Some((w, h)) = view.preview.texture_size {
                    ui.weak(format!("• {}x{} px", w, h));
                }
                let dark_theme = view.theme.is_dark();
                let mode = view.settings.dark_pages;
                let offered = match &view.preview.shown {
                    crate::preview::Shown::Raster(_) => mode.applies(true, dark_theme),
                    crate::preview::Shown::Rich(r) => r.doc.sheets.is_none() && mode.applies(false, dark_theme),
                    _ => false,
                };
                if offered {
                    let mut dark = !view.preview.show_original.get();
                    if ui.toggle_value(&mut dark, "Dark pages").on_hover_text("Off: the pages as they print (darkPages in explorer.local.jsonc)").changed() {
                        view.preview.show_original.set(!dark);
                        ui.ctx().request_repaint();
                    }
                }
            });
            if let Some(info) = file_info {
                ui.weak(info);
            }
            let sheet = match &view.preview.shown {
                crate::preview::Shown::Rich(r) if r.doc.sheets.is_some() => Some(r.clone()),
                _ => None,
            };
            if view.preview_focused {
                // A PDF's pages are shown one at a time, so Page Up and Down turn them; a video's
                // hundred "pages" are its seek steps, and stay with Ctrl.
                let one_at_a_time = matches!(view.preview.shown, crate::preview::Shown::Raster(_)) && *total_pages > 1 && *total_pages != 100;
                let step = preview_keys(ui, one_at_a_time);
                let to = asked_page as i64 + step;
                if step != 0 && to >= 0 && to < *total_pages as i64 {
                    view.want_preview.set(Some((p.clone(), to as u32)));
                    ui.ctx().request_repaint();
                }
            }
            if *total_pages > 1 {
                if let Some(book) = sheet {
                    // A workbook: the pages are its sheets, by name.
                    ui.horizontal(|ui| {
                        if ui.add_enabled(asked_page > 0, egui::Button::new("<")).clicked() {
                            view.want_preview.set(Some((p.clone(), asked_page - 1)));
                            ui.ctx().request_repaint();
                        }
                        let name = book.sheet_name(asked_page).unwrap_or("");
                        ui.label(format!("{name} ({} of {})", asked_page + 1, total_pages));
                        if ui.add_enabled(asked_page + 1 < *total_pages, egui::Button::new(">")).clicked() {
                            view.want_preview.set(Some((p.clone(), asked_page + 1)));
                            ui.ctx().request_repaint();
                        }
                    });
                } else if *total_pages == 100 {
                    ui.horizontal(|ui| {
                        ui.label("Seek:");
                        let mut pct = asked_page as f32;
                        let w = ui.available_width();
                        ui.spacing_mut().slider_width = w - ui.spacing().item_spacing.x;
                        if ui.add(egui::Slider::new(&mut pct, 0.0..=99.0).show_value(false)).changed() {
                            view.want_preview.set(Some((p.clone(), pct as u32)));
                            ui.ctx().request_repaint();
                        }
                    });
                } else {
                    ui.horizontal(|ui| {
                        if ui.button("<").clicked() {
                            let prev = asked_page.saturating_sub(1);
                            view.want_preview.set(Some((p.clone(), prev)));
                            ui.ctx().request_repaint();
                        }
                        ui.label(format!("Page {} of {}", asked_page + 1, total_pages));
                        if ui.button(">").clicked() {
                            let nxt = (asked_page + 1).min(total_pages - 1);
                            view.want_preview.set(Some((p.clone(), nxt)));
                            ui.ctx().request_repaint();
                        }
                    });
                }
            }
        }
    }
}

fn shellexpand_home(s: &str) -> String {
    if let Some(rest) = s.strip_prefix("~/") {
        if let Some(home) = std::env::var_os("HOME") {
            return format!("{}/{rest}", home.to_string_lossy());
        }
    }
    s.to_string()
}

fn _test_drag_btn(resp: &egui::Response) {
    let _b = resp.drag_started_by(egui::PointerButton::Secondary);
}

/// Line numbers for `text`, in the monospace size the text is drawn in, to stand beside it.
pub fn line_numbers(ui: &mut egui::Ui, text: &str, theme: &Theme) {
    let lines = text.split('\n').count().max(1);
    let digits = lines.to_string().len();
    let numbers: String = (1..=lines).map(|n| format!("{n:>digits$}")).collect::<Vec<_>>().join("\n");
    ui.add(egui::Label::new(egui::RichText::new(numbers).font(egui::FontId::monospace(12.0)).color(theme.text.gamma_multiply(0.35))).selectable(false));
    ui.add_space(6.0);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A dark palette keeps the light red and its own accent for letters; a light one gets its
    /// own error color and an accent dark enough to read on the window.
    #[test]
    fn letters_in_the_accent_and_errors_follow_the_palette() {
        let theme = |json: serde_json::Value| Theme::from_config(&Config::from_value(&json));
        let dark = theme(serde_json::json!({ "color": { "background": "#ff202326", "text": "#fcfcfc", "accent": "#3daee9", "error": "#da4453" } }));
        assert_eq!((dark.accent_text, dark.error), (dark.accent, egui::Color32::LIGHT_RED));
        let light = theme(serde_json::json!({ "color": { "background": "#ffeff0f1", "text": "#232629", "accent": "#3daee9", "error": "#da4453" } }));
        let window = egui::Color32::from_rgb(0xef, 0xf0, 0xf1);
        assert_eq!(light.accent, egui::Color32::from_rgb(0x3d, 0xae, 0xe9), "fills keep the accent");
        assert!(vanta_ui::theme::contrast(light.accent_text, window) >= 3.0);
        assert_eq!(light.error, egui::Color32::from_rgb(0xda, 0x44, 0x53));
    }

    fn cols_for(width: f32) -> Cols {
        let rect = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(width, 20.0));
        // Deliberately absurd stored widths: the pane, not the saved preference, must win.
        Cols::new(rect, 420.0, 90.0, NEED)
    }

    /// What a narrow font needs: the old fixed minimums.
    const NEED: ColNeed = ColNeed { size: MIN_SIZE_W, modified: MIN_MODIFIED_W };

    /// The bug this guards: with four panes beside a wide preview panel each pane fell to
    /// ~80px, the forced minimums pushed Modified past the pane's right edge, and it painted
    /// on top of the neighbouring pane's rows.
    #[test]
    fn columns_never_escape_the_pane() {
        for w in [40, 60, 82, 100, 140, 141, 200, 400, 1200] {
            let c = cols_for(w as f32);
            let limit = c.right + 0.01;
            assert!(c.name_left <= limit, "w={w}: name_left {} > {limit}", c.name_left);
            if c.show_size {
                assert!(c.size_left <= limit, "w={w}: size_left {} > {limit}", c.size_left);
            }
            if c.show_modified {
                assert!(
                    c.modified_left + c.modified_w <= limit,
                    "w={w}: modified ends at {} > {limit}",
                    c.modified_left + c.modified_w
                );
            }
        }
    }

    #[test]
    fn narrow_panes_drop_columns_widest_first() {
        // Too narrow for anything but the name.
        let c = cols_for(80.0);
        assert!(!c.show_size && !c.show_modified);

        // Room for Name + Size but not Modified.
        let c = cols_for(MIN_NAME_W + MIN_SIZE_W + 12.0);
        assert!(c.show_size, "Size should survive before Modified does");
        assert!(!c.show_modified);

        // Roomy: everything on.
        let c = cols_for(600.0);
        assert!(c.show_size && c.show_modified);
    }

    /// With Modified hidden, rows right-align Size against `modified_left`; it must therefore
    /// sit at the pane edge rather than at a stale mid-pane boundary.
    #[test]
    fn hidden_modified_lets_size_reach_the_edge() {
        let c = cols_for(MIN_NAME_W + MIN_SIZE_W + 12.0);
        assert!(!c.show_modified);
        assert_eq!(c.modified_left, c.right);
        assert_eq!(c.modified_w, 0.0);
    }

    /// A wide font: Size gets the room its text needs rather than being squeezed to "…KiB",
    /// and Modified gives way before the names are cut to a few letters.
    #[test]
    fn a_wide_font_keeps_sizes_whole_and_drops_dates_first() {
        let need = ColNeed { size: 90.0, modified: 150.0 };
        let rect = |w: f32| egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(w, 20.0));

        let c = Cols::new(rect(350.0), 420.0, 40.0, need);
        assert!(c.show_size && !c.show_modified);
        assert!(c.size_w >= need.size);
        assert!(c.name_w >= NAME_BEFORE_MODIFIED);

        let c = Cols::new(rect(600.0), 420.0, 40.0, need);
        assert!(c.show_size && c.show_modified);
        assert!(c.size_w >= need.size && c.modified_w >= need.modified);
    }
}

#[cfg(test)]
mod menu_marking_tests {
    /// A menu opened from the keys (F10) is marked on its first item from the first frame the
    /// marking shows: never over the whole menu, which its own area and a widget over all of
    /// it are, until the items are known.
    #[test]
    fn the_keys_marking_starts_on_the_first_item() {
        let ctx = egui::Context::default();
        let ring = ctx.global_style().visuals.selection.stroke.color;
        let mut first = None;
        for frame in 0..6 {
            let mut out = ctx.run_ui(egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1200.0, 800.0))), ..Default::default() }, |ui| {
                let resp = ui.button("target");
                super::context_popup(&resp, None, (frame == 0).then_some(egui::pos2(100.0, 100.0)), |ui| {
                    for t in ["Open", "Copy", "Cut", "Rename", "Properties"] {
                        let _ = ui.button(t);
                    }
                    first = ctx_first_item(ui);
                });
            });
            for c in &out.shapes {
                if let egui::Shape::Rect(r) = &c.shape {
                    if r.stroke.color == ring && r.stroke.width == 1.5 {
                        assert!(r.rect.height() < 30.0, "frame {frame}: marking over {:?}", r.rect);
                        assert_eq!(Some(r.rect.min.y), first.map(|f: egui::Rect| f.min.y), "frame {frame}");
                    }
                }
            }
            out.textures_delta.clear();
        }
    }

    /// The first item's place, as the menu's ui has it after the items.
    fn ctx_first_item(ui: &egui::Ui) -> Option<egui::Rect> {
        let layer = ui.layer_id();
        ui.ctx().viewport(|v| v.this_pass.widgets.get_layer(layer).filter(|w| w.rect.height() < 30.0).map(|w| w.rect).min_by(|a, b| a.min.y.total_cmp(&b.min.y)))
    }
}

#[cfg(test)]
mod submenu_key_tests {
    fn tap(k: egui::Key) -> Vec<egui::Event> {
        vec![
            egui::Event::Key { key: k, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::NONE },
            egui::Event::Key { key: k, physical_key: None, pressed: false, repeat: false, modifiers: egui::Modifiers::NONE },
        ]
    }

    /// Steers a menu with a submenu from the keys, opened by F10 or by a right-click with the
    /// pointer left resting on another item, checking after each key where the keys are and
    /// whether the submenu is open.
    fn steer(mouse: bool) {
        let ctx = egui::Context::default();
        let screen = Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1200.0, 800.0)));
        let target = std::cell::Cell::new(egui::Rect::NOTHING);
        let frame = |events: Vec<egui::Event>, key_at: Option<egui::Pos2>| {
            let mut out = ctx.run_ui(egui::RawInput { screen_rect: screen, events, ..Default::default() }, |ui| {
                let resp = ui.add(egui::Button::new("target").sense(egui::Sense::click()));
                target.set(resp.rect);
                super::context_popup(&resp, super::right_click_opens(&resp), key_at, |ui| {
                    let _ = ui.button("Open");
                    super::submenu(ui, "Open with", |ui| {
                        let _ = ui.button("App A");
                        let _ = ui.button("App B");
                    });
                    let _ = ui.button("Copy");
                    let _ = ui.button("Rename");
                });
            });
            out.textures_delta.clear();
        };
        frame(vec![], None);
        if mouse {
            let p = target.get().center();
            frame(vec![egui::Event::PointerMoved(p)], None);
            frame(vec![egui::Event::PointerButton { pos: p, button: egui::PointerButton::Secondary, pressed: true, modifiers: Default::default() }], None);
            frame(vec![egui::Event::PointerButton { pos: p, button: egui::PointerButton::Secondary, pressed: false, modifiers: Default::default() }], None);
            // Left resting on "Rename".
            frame(vec![egui::Event::PointerMoved(p + egui::vec2(30.0, 75.0))], None);
        } else {
            frame(vec![], Some(egui::pos2(100.0, 100.0)));
        }
        for _ in 0..3 {
            frame(vec![], None);
        }
        // Two levels with items on them: the menu and its submenu.
        let open = || ctx.viewport(|v| v.prev_pass.widgets.layer_ids().filter(|l| l.order == egui::Order::Foreground && v.prev_pass.widgets.get_layer(*l).next().is_some()).count() == 2);
        use egui::Key::*;
        let mut script = vec![(ArrowDown, vec![1], false), (ArrowRight, vec![1, 0], true), (ArrowLeft, vec![1], false), (ArrowRight, vec![1, 0], true), (ArrowDown, vec![1, 1], true), (ArrowLeft, vec![1], false), (ArrowRight, vec![1, 0], true), (ArrowLeft, vec![1], false), (ArrowDown, vec![2], false)];
        if mouse {
            // The first key takes the menu over, on its first item.
            script.insert(0, (ArrowDown, vec![0], false));
        }
        for (n, (key, path, sub)) in script.into_iter().enumerate() {
            frame(tap(key), None);
            for _ in 0..3 {
                frame(vec![], None);
            }
            let k = ctx.data(|d| d.get_temp::<super::KeyMenu>(egui::Id::new(super::KEY_MENU))).expect("menu open");
            assert_eq!(k.path, path, "step {n} ({key:?}), mouse {mouse}");
            assert_eq!(open(), sub, "step {n} ({key:?}), mouse {mouse}: submenu open");
        }
    }

    #[test]
    fn right_and_left_open_and_close_a_submenu_every_time() {
        steer(false);
        steer(true);
    }
}
