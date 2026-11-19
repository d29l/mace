//! `mace.ui`: a plugin's menu is plain tables made by the builders below; when
//! it's drawn, Mace turns them into the panel's `Node`s, giving each callback
//! a key the panel sends back when that control is used.

use mlua::{Function, Lua, Table, Value as LuaValue};

use super::api::{Ctx, own_file};
use super::protocol::FromHost;
use mace_proto::{Menu, Node};

/// The builders, in Luau: each makes a table with a `type`.
pub const PRELUDE: &str = r#"
local ui = {}
local function options(o)
    if o ~= nil and type(o) ~= "table" then error("options must be a table", 3) end
    return o or {}
end
function ui.menu(props, children)
    props = options(props)
    return { type = "menu", title = props.title, subtitle = props.subtitle, children = children or props.children or {} }
end
function ui.text(text, o)
    o = options(o)
    return { type = "text", text = tostring(text), style = o.style }
end
function ui.button(label, o)
    o = options(o)
    return { type = "button", label = tostring(label), style = o.style, icon = o.icon, onClick = o.onClick }
end
function ui.row(o)
    o = options(o)
    return { type = "row", icon = o.icon, image = o.image, title = o.title, subtitle = o.subtitle,
             onClick = o.onClick, trailing = o.trailing }
end
function ui.toggle(label, o)
    o = options(o)
    return { type = "toggle", label = tostring(label), value = o.value == true, onChange = o.onChange }
end
function ui.input(o)
    o = options(o)
    return { type = "input", placeholder = o.placeholder, value = o.value, onSubmit = o.onSubmit }
end
function ui.stack(children, o)
    o = options(o)
    return { type = "stack", children = children or {}, direction = o.direction, gap = o.gap }
end
function ui.space(size)
    return { type = "space", size = size }
end
function ui.progress(value, max, o)
    o = options(o)
    return { type = "progress", value = value, max = max, rate = o.rate, times = o.times == true }
end
function ui.image(image, o)
    o = options(o)
    return { type = "image", image = image, size = o.size }
end
return ui
"#;

/// Blocks a menu may have, and how deep.
const MOST_NODES: usize = 300;
const DEEPEST: usize = 12;
const LONGEST_TEXT: usize = 500;

fn bad(text: impl Into<String>) -> mlua::Error {
    mlua::Error::runtime(text.into())
}

/// A built-in icon's name, or a PNG of the plugin's (sent to the panel once):
/// what the panel is told to draw.
pub fn icon(_lua: &Lua, ctx: &Ctx, name: &str) -> mlua::Result<String> {
    if mace_proto::ICONS.contains(&name) {
        return Ok(name.to_string());
    }
    if !name.ends_with(".png") {
        return Err(bad(format!("no icon \"{name}\": use a built-in one (see docs/plugins.md) or a .png of the plugin's")));
    }
    picture(ctx, name)
}

/// A PNG in the plugin's folder, sent to the panel once, as the key it's known by.
fn picture(ctx: &Ctx, file: &str) -> mlua::Result<String> {
    // or one from the plugin's sites
    if file.starts_with("https://") {
        return ctx.web_picture(file);
    }
    let key = format!("plugin:{}/{file}", ctx.id);
    if ctx.images.borrow().contains(&key) {
        return Ok(key);
    }
    let path = own_file(&ctx.dir, file)?;
    let (size, rgba) = crate::panel::avatar_rgba(&path, 96)
        .ok_or_else(|| bad(format!("{file}: not a PNG the panel can show")))?;
    ctx.send(FromHost::Image { key: key.clone(), size, rgba: mace_proto::base64(&rgba) });
    ctx.images.borrow_mut().insert(key.clone());
    Ok(key)
}

pub struct Keys<'a> {
    pub table: &'a Table,
    pub ctx: &'a Ctx,
    pub count: usize,
}

impl Keys<'_> {
    fn key(&mut self, f: Option<Function>) -> mlua::Result<Option<String>> {
        let Some(f) = f else { return Ok(None) };
        let key = format!("k{}", self.ctx.next());
        self.table.set(key.as_str(), f)?;
        Ok(Some(key))
    }
}

fn text(table: &Table, field: &str) -> mlua::Result<Option<String>> {
    Ok(match table.get::<LuaValue>(field)? {
        LuaValue::Nil => None,
        LuaValue::String(s) => Some(s.to_str()?.chars().take(LONGEST_TEXT).collect()),
        LuaValue::Integer(n) => Some(n.to_string()),
        LuaValue::Number(n) => Some(n.to_string()),
        other => return Err(bad(format!("{field} should be text, not {}", other.type_name()))),
    })
}

fn style(table: &Table, field: &str) -> mlua::Result<String> {
    Ok(text(table, field)?.unwrap_or_default())
}

fn children(lua: &Lua, table: &Table, keys: &mut Keys, depth: usize) -> mlua::Result<Vec<Node>> {
    let list = match table.get::<LuaValue>("children")? {
        LuaValue::Nil => return Ok(Vec::new()),
        LuaValue::Table(list) => list,
        other => return Err(bad(format!("children should be a list, not {}", other.type_name()))),
    };
    let mut nodes = Vec::new();
    for child in list.sequence_values::<LuaValue>() {
        nodes.push(node(lua, child?, keys, depth + 1)?);
    }
    Ok(nodes)
}

fn node(lua: &Lua, value: LuaValue, keys: &mut Keys, depth: usize) -> mlua::Result<Node> {
    keys.count += 1;
    if keys.count > MOST_NODES {
        return Err(bad(format!("a menu has at most {MOST_NODES} blocks")));
    }
    if depth > DEEPEST {
        return Err(bad("a menu is nested too deep"));
    }
    let t = match value {
        LuaValue::Table(t) => t,
        LuaValue::String(s) => return Ok(Node::Text { text: s.to_str()?.chars().take(LONGEST_TEXT).collect(), style: String::new() }),
        other => return Err(bad(format!("a menu's blocks come from mace.ui, not {}", other.type_name()))),
    };
    let kind = text(&t, "type")?.unwrap_or_default();
    let icon_of = |field: &str| -> mlua::Result<Option<String>> {
        text(&t, field)?.map(|i| icon(lua, keys.ctx, &i)).transpose()
    };
    Ok(match kind.as_str() {
        "text" => Node::Text { text: text(&t, "text")?.unwrap_or_default(), style: style(&t, "style")? },
        "button" => Node::Button {
            label: text(&t, "label")?.unwrap_or_default(),
            style: style(&t, "style")?,
            icon: icon_of("icon")?,
            key: keys.key(t.get("onClick")?)?,
        },
        "row" => Node::Row {
            icon: icon_of("icon")?,
            image: text(&t, "image")?.map(|file| picture(keys.ctx, &file)).transpose()?,
            title: text(&t, "title")?.unwrap_or_default(),
            subtitle: text(&t, "subtitle")?,
            key: keys.key(t.get("onClick")?)?,
            trailing: match t.get::<LuaValue>("trailing")? {
                LuaValue::Nil => None,
                trailing => Some(Box::new(node(lua, trailing, keys, depth + 1)?)),
            },
        },
        "toggle" => Node::Toggle {
            label: text(&t, "label")?.unwrap_or_default(),
            value: t.get::<Option<bool>>("value")?.unwrap_or(false),
            key: keys.key(t.get("onChange")?)?,
        },
        "input" => Node::Input {
            placeholder: text(&t, "placeholder")?.unwrap_or_default(),
            value: text(&t, "value")?.unwrap_or_default(),
            key: keys.key(t.get("onSubmit")?)?,
        },
        "stack" => Node::Stack {
            direction: style(&t, "direction")?,
            gap: style(&t, "gap")?,
            children: children(lua, &t, keys, depth)?,
        },
        "space" => Node::Space { size: style(&t, "size")? },
        "progress" => Node::Progress {
            value: t.get::<Option<f64>>("value")?.unwrap_or(0.0),
            max: t.get::<Option<f64>>("max")?.unwrap_or(1.0),
            rate: t.get::<Option<f64>>("rate")?.unwrap_or(0.0),
            times: t.get::<Option<bool>>("times")?.unwrap_or(false),
        },
        "image" => Node::Image {
            image: picture(keys.ctx, &text(&t, "image")?.unwrap_or_default())?,
            size: style(&t, "size")?,
        },
        "menu" => return Err(bad("ui.menu goes around a menu, not inside one")),
        "" => return Err(bad("a menu's blocks come from mace.ui (this table has no type)")),
        other => return Err(bad(format!("no block \"{other}\" in mace.ui"))),
    })
}

/// What a menu function gave, as the panel draws it.
pub fn to_menu(lua: &Lua, value: LuaValue, keys: &mut Keys) -> mlua::Result<Menu> {
    let LuaValue::Table(t) = value else {
        return Err(bad("the menu function should return ui.menu(...)"));
    };
    if text(&t, "type")?.as_deref() != Some("menu") {
        return Err(bad("the menu function should return ui.menu(...)"));
    }
    Ok(Menu {
        title: text(&t, "title")?.unwrap_or_else(|| keys.ctx.name.clone()),
        subtitle: text(&t, "subtitle")?,
        children: children(lua, &t, keys, 0)?,
    })
}
