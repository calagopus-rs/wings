use super::ServerConfigurationFile;

pub struct XmlFileParser;

#[async_trait::async_trait]
impl super::ProcessConfigurationFileParser for XmlFileParser {
    async fn process_file(
        content: &str,
        config: &ServerConfigurationFile,
        server: &crate::server::Server,
    ) -> Result<Vec<u8>, anyhow::Error> {
        tracing::debug!(
            server = %server.uuid,
            "processing xml file"
        );

        let content = if content.trim().is_empty() {
            r#"<?xml version="1.0" encoding="UTF-8"?><root></root>"#
        } else {
            content
        };

        let mut root = xmltree::Element::parse(content.as_bytes())?;

        for replacement in &config.replace {
            let value = ServerConfigurationFile::replace_all_placeholders(
                server,
                &replacement.replace_with,
            )
            .await?;

            let path = replacement.r#match.replace('.', "/");
            let path_parts: Vec<&str> = path.split('/').filter(|p| !p.is_empty()).collect();

            if path.contains('*') {
                update_xml_wildcard(
                    &mut root,
                    &path_parts,
                    &value,
                    replacement.insert_new.unwrap_or(true),
                    replacement.update_existing,
                );
            } else {
                update_xml_element(
                    &mut root,
                    &path_parts,
                    &value,
                    replacement.insert_new.unwrap_or(true),
                    replacement.update_existing,
                );
            }
        }

        let mut result = Vec::new();
        root.write_with_config(
            &mut result,
            xmltree::EmitterConfig::new()
                .perform_indent(true)
                .indent_string("  "),
        )?;

        Ok(result)
    }
}

fn update_xml_element(
    element: &mut xmltree::Element,
    path: &[&str],
    value: &str,
    insert_new: bool,
    update_existing: bool,
) {
    if path.is_empty() {
        return;
    }

    if path.len() == 1 {
        let tag = path[0];

        if let Some(attr_value) = value.strip_prefix('@') {
            if let Some(eq_pos) = attr_value.find('=') {
                let attr_name = &attr_value[..eq_pos];
                let attr_val = &attr_value[eq_pos + 1..];

                let exists = element.attributes.contains_key(attr_name);
                if (exists && update_existing) || (!exists && insert_new) {
                    element
                        .attributes
                        .insert(attr_name.to_string(), attr_val.to_string());
                }
            }
            return;
        }

        if let Some(child) = element.get_mut_child(tag) {
            if update_existing {
                child.children.clear();
                child
                    .children
                    .push(xmltree::XMLNode::Text(value.to_string()));
            }
        } else if insert_new {
            let mut new_child = xmltree::Element::new(tag);
            new_child
                .children
                .push(xmltree::XMLNode::Text(value.to_string()));
            element.children.push(xmltree::XMLNode::Element(new_child));
        }
        return;
    }

    let tag = path[0];
    if let Some(child) = element.get_mut_child(tag) {
        update_xml_element(child, &path[1..], value, insert_new, update_existing);
    } else if insert_new {
        let mut new_child = xmltree::Element::new(tag);
        update_xml_element(
            &mut new_child,
            &path[1..],
            value,
            insert_new,
            update_existing,
        );
        element.children.push(xmltree::XMLNode::Element(new_child));
    }
}

fn update_xml_wildcard(
    element: &mut xmltree::Element,
    path: &[&str],
    value: &str,
    insert_new: bool,
    update_existing: bool,
) {
    if path.is_empty() {
        return;
    }

    let tag = path[0];

    let should_check_insertion = tag != "*" && insert_new;
    let mut found_match = false;

    for child in &mut element.children {
        let xmltree::XMLNode::Element(child_elem) = child else {
            continue;
        };

        let matches = tag == "*" || child_elem.name == tag;
        if !matches {
            continue;
        }

        found_match = true;

        if path.len() == 1 {
            if update_existing {
                child_elem.children.clear();
                child_elem
                    .children
                    .push(xmltree::XMLNode::Text(value.to_string()));
            }
        } else {
            update_xml_wildcard(child_elem, &path[1..], value, insert_new, update_existing);
        }
    }

    if !found_match && should_check_insertion {
        if path.len() == 1 {
            let mut new_child = xmltree::Element::new(tag);
            new_child
                .children
                .push(xmltree::XMLNode::Text(value.to_string()));
            element.children.push(xmltree::XMLNode::Element(new_child));
        } else {
            let mut new_child = xmltree::Element::new(tag);
            update_xml_wildcard(
                &mut new_child,
                &path[1..],
                value,
                insert_new,
                update_existing,
            );
            element.children.push(xmltree::XMLNode::Element(new_child));
        }
    }
}
