use std::{collections::BTreeMap, path::PathBuf};

use glib;
use gst::prelude::ObjectExt;

#[derive(Default, serde::Serialize, serde::Deserialize, Debug)]
#[serde(default)]
struct PropertyInfo {
    name: String,
    nick: String,
    blurb: Option<String>,
    value_type: String,
    owner_type: String,
    flags: String,
    #[serde(skip)]
    default_value: Option<glib::value::Value>,
}

#[derive(Debug, serde::Deserialize, serde::Serialize)]
struct PluginInfo {
    plugin_name: String,
    description: String,
    filename: Option<PathBuf>,
    license: String,
    origin: String,
    package: String,
    release_date_string: Option<String>,
    source: String,
    version: String,
    is_loaded: bool,
    properties: BTreeMap<String, PropertyInfo>,
}

fn get_plugin_properties(plugin: &gst::Plugin) -> BTreeMap<String, PropertyInfo> {
    let name = plugin.plugin_name();

    // It's necessary to create the element from a factory to access the inner properties
    // Otherwise only `name` and `parent` are accessible
    let mut properties = BTreeMap::new();
    let Some(factory) = gst::ElementFactory::find(&name) else {
        return properties;
    };
    let Ok(element) = factory.create().build() else {
        return properties;
    };

    for property in element.list_properties() {
        let name = property.name().to_string();
        // There is no reason to display those
        if ["name", "parent"].contains(&name.as_ref()) {
            continue;
        }
        let property_info = PropertyInfo {
            name: name.clone(),
            nick: property.nick().to_string(),
            blurb: property.blurb().map(|s| s.to_string()),
            value_type: property.value_type().name().to_string(),
            owner_type: property.owner_type().name().to_string(),
            flags: format!("{:?}", property.flags()),
            default_value: Some(property.default_value().clone()),
        };
        properties.insert(name, property_info);
    }

    properties
}

#[derive(serde::Deserialize, serde::Serialize)]
#[serde(default)]
pub struct Info {
    plugins_info: BTreeMap<String, PluginInfo>,
}

impl Default for Info {
    fn default() -> Self {
        let registry = gst::Registry::get();
        let mut plugins_info = BTreeMap::new();

        for plugin in registry.plugins() {
            let info = PluginInfo {
                plugin_name: plugin.plugin_name().to_string(),
                description: plugin.description().to_string(),
                filename: plugin.filename(),
                license: plugin.license().to_string(),
                origin: plugin.origin().to_string(),
                package: plugin.package().to_string(),
                release_date_string: plugin.release_date_string().map(|d| d.to_string()),
                source: plugin.source().to_string(),
                version: plugin.version().to_string(),
                is_loaded: plugin.is_loaded(),
                properties: get_plugin_properties(&plugin),
            };
            plugins_info.insert(plugin.plugin_name().to_string(), info);
        }

        Self { plugins_info }
    }
}
