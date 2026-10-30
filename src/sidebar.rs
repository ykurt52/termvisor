// Activity bar and side panel.

use std::cell::RefCell;
use std::rc::Rc;

use gtk::{gio, glib, prelude::*};

use crate::sessions::{self, Node};
use crate::term::Launch;

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
    pub fn new(open: Rc<dyn Fn(Launch)>) -> Self {
        let bar = gtk::Box::new(gtk::Orientation::Vertical, 0);
        bar.add_css_class("activitybar");

        let stack = gtk::Stack::new();
        stack.set_transition_type(gtk::StackTransitionType::Crossfade);
        stack.set_vexpand(true);

        let mut buttons = Vec::new();
        for &(id, icon, title, note) in PAGES {
            let page = match note {
                None => sessions_page(open.clone()).upcast::<gtk::Widget>(),
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

/// Icon color class for a tree icon.
fn tint(icon: &str) -> &'static str {
    match icon {
        "utilities-terminal-symbolic" => "tint-blue",
        "network-server-symbolic" => "tint-green",
        "folder-symbolic" => "tint-yellow",
        _ => "tint-cyan",
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

fn sessions_page(open: Rc<dyn Fn(Launch)>) -> gtk::Box {
    let all = Rc::new(RefCell::new(sessions::tree()));
    let root = store(&all.borrow());

    let tree = gtk::TreeListModel::new(root.clone(), false, true, |obj| {
        match node_of(Some(obj.clone()))? {
            Node::Folder { kids, .. } => Some(store(&kids).upcast()),
            Node::Item { .. } => None,
        }
    });

    let factory = gtk::SignalListItemFactory::new();
    factory.connect_setup(|_, obj| {
        let Some(item) = obj.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        row.append(&gtk::Image::new());
        row.append(&gtk::Label::new(None));
        let exp = gtk::TreeExpander::new();
        exp.set_child(Some(&row));
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
        icon.set_icon_name(Some(node.icon()));
        icon.set_css_classes(&[tint(node.icon())]);
        label.set_text(node.name());
    });

    let list = gtk::ListView::new(Some(gtk::SingleSelection::new(Some(tree))), Some(factory));
    list.connect_activate(move |lv, pos| {
        let Some(row) = lv.model().and_then(|m| m.item(pos)).and_downcast::<gtk::TreeListRow>() else {
            return;
        };
        match node_of(row.item()) {
            Some(Node::Item { launch, .. }) => open(launch),
            Some(Node::Folder { .. }) => row.set_expanded(!row.is_expanded()),
            None => {}
        }
    });

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
    refresh.set_tooltip_text(Some("Reload ~/.ssh/config"));
    refresh.connect_clicked(move |_| {
        all.replace(sessions::tree());
        fill();
    });
    let add = gtk::Button::from_icon_name("list-add-symbolic");
    add.add_css_class("flat");
    add.set_sensitive(false);
    add.set_tooltip_text(Some("Session editor: milestone 2"));

    let tools = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    tools.set_halign(gtk::Align::End);
    tools.set_margin_end(4);
    tools.append(&add);
    tools.append(&refresh);

    let scroll = gtk::ScrolledWindow::new();
    scroll.set_vexpand(true);
    scroll.set_child(Some(&list));

    let page = gtk::Box::new(gtk::Orientation::Vertical, 0);
    page.append(&tools);
    page.append(&search);
    page.append(&scroll);
    page
}
