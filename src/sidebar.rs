// Activity bar and side panel.

use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gio, glib};

use crate::editor::{self, Mode};
use crate::sessions::{self, Node, NodeKind};
use crate::state::State;
use crate::store::{self, Entry, Folder, Ssh};
use crate::keyring;
use crate::term::{Kind, Launch};

/// Opens a session or reports why it can't.
pub type OpenFn = Rc<dyn Fn(Result<Launch, String>)>;

/// id, icon, title, placeholder text (None = real page).
const PAGES: &[(&str, &str, &str, Option<&str>)] = &[
    ("sessions", "network-server-symbolic", "Sessions", None),
    ("files", "folder-symbolic", "Files", Some("SFTP and FTP browser. Planned for milestone 7.")),
    ("tunnels", "network-transmit-receive-symbolic", "Tunnels", Some("SSH tunnel manager. Planned for milestone 10.")),
    ("macros", "media-record-symbolic", "Macros", Some("Recorded and scripted macros. Planned for milestone 10.")),
    ("tools", "applications-utilities-symbolic", "Tools", Some("TFTP, HTTP server, port scanner, ping. Planned for milestone 11.")),
];

#[derive(Clone)]
pub struct Sidebar {
    pub bar: gtk::Box,
    pub panel: gtk::Box,
    stack: gtk::Stack,
    buttons: Rc<Vec<(&'static str, gtk::Button)>>,
}

impl Sidebar {
    pub fn new(state: &Rc<State>, open: OpenFn) -> Self {
        let bar = gtk::Box::new(gtk::Orientation::Vertical, 0);
        bar.add_css_class("activitybar");

        let stack = gtk::Stack::new();
        stack.set_transition_type(gtk::StackTransitionType::Crossfade);
        stack.set_vexpand(true);

        let mut buttons = Vec::new();
        for &(id, icon, title, note) in PAGES {
            let page = match note {
                None => sessions_page(state, open.clone()).upcast::<gtk::Widget>(),
                Some(text) => placeholder(icon, text).upcast::<gtk::Widget>(),
            };
            stack.add_named(&titled(title, &page), Some(id));

            let b = gtk::Button::from_icon_name(icon);
            b.add_css_class("flat");
            b.add_css_class(&format!("act-{id}"));
            b.set_tooltip_text(Some(title));
            bar.append(&b);
            buttons.push((id, b));
        }

        let panel = gtk::Box::new(gtk::Orientation::Vertical, 0);
        panel.add_css_class("sidebar");
        panel.set_size_request(240, -1);
        panel.append(&stack);

        let sb = Self { bar, panel, stack, buttons: Rc::new(buttons) };
        for (id, b) in sb.buttons.iter() {
            let s = sb.clone();
            let id = *id;
            b.connect_clicked(move |_| s.select(id));
        }
        sb.mark();
        sb
    }

    /// Shows page `id`; clicking the open page again hides the panel.
    pub fn select(&self, id: &str) {
        let open = self.panel.is_visible() && self.stack.visible_child_name().as_deref() == Some(id);
        if open {
            self.panel.set_visible(false);
        } else {
            self.stack.set_visible_child_name(id);
            self.panel.set_visible(true);
        }
        self.mark();
    }

    pub fn toggle(&self) {
        self.panel.set_visible(!self.panel.is_visible());
        self.mark();
    }

    fn mark(&self) {
        let cur = self.stack.visible_child_name();
        for (id, b) in self.buttons.iter() {
            if self.panel.is_visible() && cur.as_deref() == Some(*id) {
                b.add_css_class("active");
            } else {
                b.remove_css_class("active");
            }
        }
    }
}

fn titled(title: &str, body: &impl IsA<gtk::Widget>) -> gtk::Box {
    let b = gtk::Box::new(gtk::Orientation::Vertical, 0);
    let l = gtk::Label::new(Some(&title.to_uppercase()));
    l.set_xalign(0.0);
    l.add_css_class("title");
    b.append(&l);
    b.append(body);
    b
}

fn placeholder(icon: &str, text: &str) -> adw::StatusPage {
    let p = adw::StatusPage::new();
    p.set_icon_name(Some(icon));
    p.set_description(Some(text));
    p.add_css_class("compact");
    p.set_vexpand(true);
    p
}

fn store(nodes: &[Node]) -> gio::ListStore {
    let s = gio::ListStore::new::<glib::BoxedAnyObject>();
    for n in nodes {
        s.append(&glib::BoxedAnyObject::new(n.clone()));
    }
    s
}

fn node_of(obj: Option<glib::Object>) -> Option<Node> {
    let boxed = obj?.downcast::<glib::BoxedAnyObject>().ok()?;
    let node = boxed.try_borrow::<Node>().ok()?;
    Some(node.clone())
}

fn menu_item(label: &str, action: &str, target: Option<&str>) -> gio::MenuItem {
    let i = gio::MenuItem::new(Some(label), None);
    match target {
        Some(t) => i.set_action_and_target_value(Some(action), Some(&t.to_variant())),
        None => i.set_detailed_action(action),
    }
    i
}

fn section(items: &[gio::MenuItem]) -> gio::Menu {
    let m = gio::Menu::new();
    for i in items {
        m.append_item(i);
    }
    m
}

/// Context menu for `node` (None = empty area).
fn row_menu(node: Option<&Node>) -> gio::Menu {
    let menu = gio::Menu::new();
    let create = |dir: &[usize], here: bool| {
        let d = store::path_str(dir);
        let suffix = if here { " Here" } else { "" };
        section(&[
            menu_item(&format!("New Session{suffix}"), "sess.new-session", Some(&d)),
            menu_item(&format!("New Folder{suffix}"), "sess.new-folder", Some(&d)),
        ])
    };
    match node.map(|n| (&n.kind, n.path.as_deref(), n.star)) {
        Some((NodeKind::Item(_), Some(path), star)) => {
            let parent = &path[..path.len().saturating_sub(1)];
            let fav = if star { "Remove from Favorites" } else { "Add to Favorites" };
            menu.append_section(None, &section(&[menu_item("Connect", "sess.connect", None)]));
            menu.append_section(
                None,
                &section(&[
                    menu_item("Edit…", "sess.edit", None),
                    menu_item("Duplicate", "sess.duplicate", None),
                    menu_item(fav, "sess.favorite", None),
                ]),
            );
            if node.is_some_and(|n| matches!(&n.kind, NodeKind::Item(Ok(l)) if l.kind == Kind::Ssh)) {
                menu.append_section(None, &section(&[menu_item("Forget Saved Password", "sess.forget", None)]));
            }
            menu.append_section(None, &create(parent, true));
            menu.append_section(None, &section(&[menu_item("Delete…", "sess.delete", None)]));
        }
        Some((NodeKind::Folder(_), Some(path), _)) => {
            menu.append_section(None, &section(&[menu_item("Edit…", "sess.edit", None)]));
            menu.append_section(None, &create(path, true));
            menu.append_section(None, &section(&[menu_item("Delete…", "sess.delete", None)]));
        }
        Some((NodeKind::Item(_), None, _)) => {
            let mut items = vec![menu_item("Connect", "sess.connect", None)];
            if node.is_some_and(|n| n.host.is_some()) {
                items.push(menu_item("Copy to Sessions", "sess.copy", None));
            }
            menu.append_section(None, &section(&items));
        }
        _ => menu.append_section(None, &create(&[], false)),
    }
    menu
}

fn popup(w: &gtk::Widget, menu: &gio::Menu, x: f64, y: f64) {
    let pop = gtk::PopoverMenu::from_model(Some(menu));
    pop.set_parent(w);
    pop.set_has_arrow(false);
    pop.set_halign(gtk::Align::Start);
    pop.set_pointing_to(Some(&gtk::gdk::Rectangle::new(x as i32, y as i32, 1, 1)));
    pop.connect_closed(|p| {
        let p = p.clone();
        glib::idle_add_local_once(move || p.unparent());
    });
    pop.popup();
}

type Handler = Box<dyn Fn(Option<&glib::Variant>)>;

/// Row actions under the "sess" prefix. They act on the node in `ctx`.
fn row_actions(state: &Rc<State>, open: &OpenFn, ctx: &Rc<RefCell<Option<Node>>>, host: &gtk::Widget) -> gio::SimpleActionGroup {
    let group = gio::SimpleActionGroup::new();
    let path_of = {
        let ctx = ctx.clone();
        move || ctx.borrow().as_ref().and_then(|n| n.path.clone())
    };
    let report = {
        let open = open.clone();
        move |r: Result<(), String>| {
            if let Err(e) = r {
                open(Err(e));
            }
        }
    };
    let add = |name: &str, param: Option<&glib::VariantTy>, f: Handler| {
        let a = gio::SimpleAction::new(name, param);
        a.connect_activate(move |_, p| f(p));
        group.add_action(&a);
    };

    let (c, o) = (ctx.clone(), open.clone());
    add("connect", None, Box::new(move |_| {
        if let Some(NodeKind::Item(l)) = c.borrow().as_ref().map(|n| n.kind.clone()) {
            o(l);
        }
    }));

    let (st, p, w) = (state.clone(), path_of.clone(), host.downgrade());
    add("edit", None, Box::new(move |_| {
        let (Some(path), Some(w)) = (p(), w.upgrade()) else { return };
        let entry = st.store.borrow().get(&path).cloned();
        if let Some(e) = entry {
            editor::open(&w, &st, Mode::Edit(path), e);
        }
    }));

    let (st, p, r) = (state.clone(), path_of.clone(), report.clone());
    add("duplicate", None, Box::new(move |_| {
        let Some(path) = p() else { return };
        let Some((last, dir)) = path.split_last() else { return };
        r(st.edit_sessions(|s| {
            let mut e = s.get(&path).cloned().ok_or("session not found")?;
            e.set_name(format!("{} (copy)", e.name()));
            s.insert(dir, Some(last + 1), e)
        }));
    }));

    let (st, p, r) = (state.clone(), path_of.clone(), report.clone());
    add("favorite", None, Box::new(move |_| {
        let Some(path) = p() else { return };
        r(st.edit_sessions(|s| {
            let mut e = s.get(&path).cloned().ok_or("session not found")?;
            e.set_favorite(!e.favorite());
            s.replace(&path, e)
        }));
    }));

    let (st, p, r, w) = (state.clone(), path_of.clone(), report.clone(), host.downgrade());
    add("delete", None, Box::new(move |_| {
        let (Some(path), Some(w)) = (p(), w.upgrade()) else { return };
        let Some(e) = st.store.borrow().get(&path).cloned() else { return };
        let body = match e.count() {
            0 => "This cannot be undone.".to_string(),
            n => format!("The folder and the {n} items inside it will be deleted."),
        };
        let d = adw::AlertDialog::new(Some(&format!("Delete “{}”?", e.name())), Some(&body));
        d.add_responses(&[("cancel", "Cancel"), ("delete", "Delete")]);
        d.set_response_appearance("delete", adw::ResponseAppearance::Destructive);
        d.set_default_response(Some("cancel"));
        d.set_close_response("cancel");
        let (st, r) = (st.clone(), r.clone());
        d.connect_response(None, move |_, resp| {
            if resp == "delete" {
                r(st.edit_sessions(|s| s.remove(&path).map(|_| ())));
            }
        });
        d.present(Some(&w));
    }));

    let (c, r) = (ctx.clone(), report.clone());
    add("forget", None, Box::new(move |_| {
        let target = match c.borrow().as_ref().map(|n| &n.kind) {
            Some(NodeKind::Item(Ok(l))) => l.target.clone(),
            _ => return,
        };
        r(keyring::clear(&keyring::password_id(&target)));
    }));

    let (st, c, r) = (state.clone(), ctx.clone(), report.clone());
    add("copy", None, Box::new(move |_| {
        let Some(host) = c.borrow().as_ref().and_then(|n| n.host.clone()) else { return };
        let e = Entry::Ssh(Ssh { name: host.clone(), host, ..Default::default() });
        r(st.edit_sessions(|s| s.insert(&[], None, e)));
    }));

    for (name, folder) in [("new-session", false), ("new-folder", true)] {
        let (st, w) = (state.clone(), host.downgrade());
        add(name, Some(glib::VariantTy::STRING), Box::new(move |v| {
            let Some(w) = w.upgrade() else { return };
            let Some(dir) = v.and_then(|v| v.get::<String>()).and_then(|d| store::path_parse(&d)) else { return };
            let init = if folder {
                Entry::Folder(Folder::default())
            } else {
                Entry::Ssh(Ssh::default())
            };
            editor::open(&w, &st, Mode::New(dir), init);
        }));
    }
    group
}

fn report(open: &OpenFn, r: Result<(), String>) {
    if let Err(e) = r {
        open(Err(e));
    }
}

fn row_node(item: &glib::WeakRef<gtk::ListItem>) -> Option<Node> {
    item.upgrade()
        .and_then(|i| i.item())
        .and_downcast::<gtk::TreeListRow>()
        .and_then(|r| node_of(r.item()))
}

/// Drag payload: the stored path as text. Built-in rows are not draggable.
fn drag_source(item: &gtk::ListItem) -> gtk::DragSource {
    let src = gtk::DragSource::new();
    src.set_actions(gtk::gdk::DragAction::MOVE);
    let weak = item.downgrade();
    src.connect_prepare(move |_, _, _| {
        let path = row_node(&weak)?.path?;
        Some(gtk::gdk::ContentProvider::for_value(&store::path_str(&path).to_value()))
    });
    src.connect_drag_begin(|s, _| {
        if let Some(w) = s.widget() {
            s.set_icon(Some(&gtk::WidgetPaintable::new(Some(&w))), 0, 0);
        }
    });
    src
}

/// Drop on a session: before or after it. On a folder: top quarter before,
/// bottom quarter after, middle into it.
fn drop_target(item: &gtk::ListItem, state: &Rc<State>, open: &OpenFn) -> gtk::DropTarget {
    let t = gtk::DropTarget::new(String::static_type(), gtk::gdk::DragAction::MOVE);
    let weak = item.downgrade();
    let (st, op) = (state.clone(), open.clone());
    t.connect_drop(move |t, v, _, y| {
        let Some(from) = v.get::<String>().ok().and_then(|f| store::path_parse(&f)) else {
            return false;
        };
        let Some(node) = row_node(&weak) else { return false };
        let Some(to) = node.path.clone() else { return false };
        let Some((&last, parent)) = to.split_last() else { return false };
        let h = t.widget().map(|w| f64::from(w.height())).unwrap_or(0.0);
        let folder = matches!(node.kind, NodeKind::Folder(_));
        let (dir, index) = if folder && y > h * 0.25 && y < h * 0.75 {
            (to.clone(), None)
        } else if y < h / 2.0 {
            (parent.to_vec(), Some(last))
        } else {
            (parent.to_vec(), Some(last + 1))
        };
        if from == to {
            return false;
        }
        report(&op, st.edit_sessions(|s| s.move_to(&from, &dir, index)));
        true
    });
    t
}

fn sessions_page(state: &Rc<State>, open: OpenFn) -> gtk::Box {
    let page = gtk::Box::new(gtk::Orientation::Vertical, 0);
    let ctx: Rc<RefCell<Option<Node>>> = Rc::default();
    page.insert_action_group("sess", Some(&row_actions(state, &open, &ctx, page.upcast_ref())));

    let all = Rc::new(RefCell::new(sessions::tree(&state.store.borrow().root, &state.recent.borrow())));
    let root = store(&all.borrow());

    let tree = gtk::TreeListModel::new(root.clone(), false, true, |obj| {
        match node_of(Some(obj.clone()))?.kind {
            NodeKind::Folder(kids) => Some(store(&kids).upcast()),
            NodeKind::Item(_) => None,
        }
    });

    let factory = gtk::SignalListItemFactory::new();
    let c = ctx.clone();
    let (st, op) = (state.clone(), open.clone());
    factory.connect_setup(move |_, obj| {
        let Some(item) = obj.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        row.append(&gtk::Image::new());
        let label = gtk::Label::new(None);
        label.set_xalign(0.0);
        label.set_hexpand(true);
        label.set_ellipsize(gtk::pango::EllipsizeMode::End);
        row.append(&label);
        let star = gtk::Image::from_icon_name("starred-symbolic");
        star.add_css_class("tint-yellow");
        row.append(&star);
        let exp = gtk::TreeExpander::new();
        exp.set_child(Some(&row));

        let click = gtk::GestureClick::new();
        click.set_button(3);
        let weak = item.downgrade();
        let c = c.clone();
        click.connect_pressed(move |g, _, x, y| {
            g.set_state(gtk::EventSequenceState::Claimed);
            let node = weak
                .upgrade()
                .and_then(|i| i.item())
                .and_downcast::<gtk::TreeListRow>()
                .and_then(|r| node_of(r.item()));
            let Some(w) = g.widget() else { return };
            let menu = row_menu(node.as_ref());
            c.replace(node);
            popup(&w, &menu, x, y);
        });
        exp.add_controller(click);
        exp.add_controller(drag_source(item));
        exp.add_controller(drop_target(item, &st, &op));
        item.set_child(Some(&exp));
    });
    factory.connect_bind(|_, obj| {
        let Some(item) = obj.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let Some(row) = item.item().and_downcast::<gtk::TreeListRow>() else {
            return;
        };
        let Some(exp) = item.child().and_downcast::<gtk::TreeExpander>() else {
            return;
        };
        exp.set_list_row(Some(&row));
        let Some(node) = node_of(row.item()) else {
            return;
        };
        let Some(icon) = exp.child().and_then(|c| c.first_child()).and_downcast::<gtk::Image>() else {
            return;
        };
        let Some(label) = icon.next_sibling().and_downcast::<gtk::Label>() else {
            return;
        };
        icon.set_icon_name(Some(node.icon));
        icon.set_css_classes(&[node.tint]);
        label.set_text(&node.name);
        if let Some(star) = label.next_sibling() {
            star.set_visible(node.star);
        }
    });

    let list = gtk::ListView::new(Some(gtk::SingleSelection::new(Some(tree))), Some(factory));
    let op = open.clone();
    list.connect_activate(move |lv, pos| {
        let Some(row) = lv.model().and_then(|m| m.item(pos)).and_downcast::<gtk::TreeListRow>() else {
            return;
        };
        match node_of(row.item()).map(|n| n.kind) {
            Some(NodeKind::Item(launch)) => op(launch),
            Some(NodeKind::Folder(_)) => row.set_expanded(!row.is_expanded()),
            None => {}
        }
    });

    let click = gtk::GestureClick::new();
    click.set_button(3);
    let c = ctx.clone();
    click.connect_pressed(move |g, _, x, y| {
        let Some(w) = g.widget() else { return };
        c.replace(None);
        popup(&w, &row_menu(None), x, y);
    });
    list.add_controller(click);

    // Drop on empty space moves to the end of the top level.
    let drop = gtk::DropTarget::new(String::static_type(), gtk::gdk::DragAction::MOVE);
    let (st, op) = (state.clone(), open.clone());
    drop.connect_drop(move |_, v, _, _| {
        let Some(from) = v.get::<String>().ok().and_then(|f| store::path_parse(&f)) else {
            return false;
        };
        report(&op, st.edit_sessions(|s| s.move_to(&from, &[], None)));
        true
    });
    list.add_controller(drop);

    let search = gtk::SearchEntry::new();
    search.set_placeholder_text(Some("Filter sessions"));
    search.set_margin_start(8);
    search.set_margin_end(8);
    search.set_margin_bottom(6);
    let fill = {
        let all = all.clone();
        let root = root.clone();
        let search = search.downgrade();
        move || {
            let q = search.upgrade().map(|s| s.text().to_string()).unwrap_or_default();
            root.remove_all();
            for n in sessions::filter(&all.borrow(), &q) {
                root.append(&glib::BoxedAnyObject::new(n));
            }
        }
    };
    let f = fill.clone();
    search.connect_search_changed(move |_| f());

    let refresh = gtk::Button::from_icon_name("view-refresh-symbolic");
    refresh.add_css_class("flat");
    refresh.set_tooltip_text(Some("Reload sessions and ~/.ssh/config"));
    let st = state.clone();
    refresh.connect_clicked(move |_| st.reload_sessions());

    // Rebuild on any session change; weak refs so closed windows drop out.
    let st = Rc::downgrade(state);
    let weak_list = list.downgrade();
    state.on_sessions(move || {
        let (Some(st), Some(_)) = (st.upgrade(), weak_list.upgrade()) else {
            return;
        };
        all.replace(sessions::tree(&st.store.borrow().root, &st.recent.borrow()));
        fill();
    });
    let add = gtk::MenuButton::new();
    add.set_icon_name("list-add-symbolic");
    add.add_css_class("flat");
    add.set_tooltip_text(Some("New Session or Folder"));
    add.set_menu_model(Some(&row_menu(None)));

    let tools = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    tools.set_halign(gtk::Align::End);
    tools.set_margin_end(4);
    tools.append(&add);
    tools.append(&refresh);

    let scroll = gtk::ScrolledWindow::new();
    scroll.set_vexpand(true);
    scroll.set_child(Some(&list));

    page.append(&tools);
    page.append(&search);
    page.append(&scroll);
    page
}
