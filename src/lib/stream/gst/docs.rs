pub fn gst_element_docs_url(factory_name: &str) -> Option<String> {
    let factory = gst::ElementFactory::find(factory_name)?;
    factory
        .documentation_uri()
        .filter(|uri| !uri.is_empty())
        .map(|uri| uri.to_string())
}

pub fn gst_property_docs_url(factory_name: &str, property: &str) -> Option<String> {
    let page = gst_element_docs_url(factory_name)?;
    if page.contains('#') {
        Some(page)
    } else {
        Some(format!("{page}#{property}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn docs_url_uses_factory_uri_only() {
        let _ = gst::init();
        let factory = gst::ElementFactory::find("x264enc").expect("x264enc");
        let provided = factory
            .documentation_uri()
            .filter(|uri| !uri.is_empty())
            .map(|uri| uri.to_string());
        assert_eq!(gst_element_docs_url("x264enc"), provided);
        assert_eq!(gst_element_docs_url("libcamerasrc"), None);
    }
}
