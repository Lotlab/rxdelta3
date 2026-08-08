//! Parser for `patch_delta_direct.dat` — a UTF-8 XML file whose entries are
//! self-closing `<XxxSubItem Key="..." Value="..."/>` elements grouped under
//! section elements like `<DeltaPathInfo>`.

use std::collections::HashMap;

use quick_xml::events::Event;
use quick_xml::Reader;

/// Section name → list of (key, value) entries, in file order.
#[derive(Debug, Default)]
pub struct PatchDat {
    sections: HashMap<String, Vec<(String, String)>>,
}

impl PatchDat {
    pub fn entries(&self, section: &str) -> &[(String, String)] {
        self.sections.get(section).map(|v| v.as_slice()).unwrap_or(&[])
    }

    pub fn count(&self, section: &str) -> usize {
        self.entries(section).len()
    }
}

/// Parse `patch_delta_direct.dat` content. Tracks the current section element
/// and collects Key/Value attributes of every `*SubItem` child.
pub fn parse(data: &[u8]) -> Result<PatchDat, String> {
    let mut reader = Reader::from_reader(data);
    reader.config_mut().trim_text(true);

    let mut dat = PatchDat::default();
    let mut current_section: Option<String> = None;

    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) => {
                let name = String::from_utf8_lossy(e.name().as_ref()).into_owned();
                if !name.is_empty() && !name.eq_ignore_ascii_case("XMLROOT") {
                    current_section = Some(name);
                }
            }
            Ok(Event::Empty(e)) => {
                let name = String::from_utf8_lossy(e.name().as_ref()).into_owned();
                if name.ends_with("SubItem") {
                    if let Some(sec) = &current_section {
                        let mut key = None;
                        let mut value = None;
                        for attr in e.attributes() {
                            let a = attr.map_err(|err| err.to_string())?;
                            let an = a.key.as_ref();
                            let av = a
                                .unescape_value()
                                .map_err(|err| err.to_string())?
                                .into_owned();
                            if an.eq_ignore_ascii_case(b"Key") {
                                key = Some(av);
                            } else if an.eq_ignore_ascii_case(b"Value") {
                                value = Some(av);
                            }
                        }
                        if let (Some(k), Some(v)) = (key, value) {
                            dat.sections.entry(sec.clone()).or_default().push((k, v));
                        }
                    }
                }
            }
            Ok(Event::Eof) => break,
            Ok(_) => {}
            Err(err) => return Err(err.to_string()),
        }
    }
    Ok(dat)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"<XMLROOT>
<DeltaPathInfo>
<DeltaPathSubItem Key="game\ffxiv_dx11.exe" Value="Pkg\game\ffxiv_dx11.exe.delta"/>
<DeltaPathSubItem Key="game\ffxivgame.ver" Value="Pkg\game\ffxivgame.ver.delta"/>
</DeltaPathInfo>
<DeltaMD5Info>
<DeltaMD5SubItem Key="game\ffxiv_dx11.exe.delta" Value="00E2745440D35D337B589CC3DC02C25E"/>
</DeltaMD5Info>
<OriginMD5Info>
<OriginMD5SubItem Key="game\ffxiv_dx11.exe" Value="04F0E75C4E67ACA6086E0DFA637F3BCC"/>
</OriginMD5Info>
<ResultMD5Info>
<ResultMD5SubItem Key="game\ffxiv_dx11.exe" Value="1342C5BC439AE0C5716C9E1E3D781A2F"/>
</ResultMD5Info>
</XMLROOT>"#;

    #[test]
    fn parses_sections_and_entries() {
        let dat = parse(SAMPLE.as_bytes()).unwrap();
        assert_eq!(dat.count("DeltaPathInfo"), 2);
        assert_eq!(dat.count("DeltaMD5Info"), 1);
        assert_eq!(dat.count("OriginMD5Info"), 1);
        assert_eq!(dat.count("ResultMD5Info"), 1);
        let d = dat.entries("DeltaPathInfo");
        assert_eq!(
            d[0],
            (
                r"game\ffxiv_dx11.exe".to_string(),
                r"Pkg\game\ffxiv_dx11.exe.delta".to_string()
            )
        );
        assert_eq!(dat.count("EmptyPathInfo"), 0);
    }

    #[test]
    fn missing_section_returns_empty() {
        let dat = parse(b"<XMLROOT></XMLROOT>").unwrap();
        assert_eq!(dat.count("DelPathInfo"), 0);
    }

    #[test]
    fn malformed_xml_errors() {
        assert!(parse(b"<XMLROOT><DeltaPathInfo><DeltaPathSubItem Key=unclosed").is_err());
    }
}
