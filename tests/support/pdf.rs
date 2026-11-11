pub fn pdf(pages: &[&str]) -> Vec<u8> {
    let mut objects = vec![
        String::new(),
        String::new(),
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_owned(),
    ];
    let mut kids = Vec::with_capacity(pages.len());
    for text in pages {
        let stream = format!("BT /F1 12 Tf 72 720 Td ({}) Tj ET", escape(text));
        objects.push(format!(
            "<< /Length {} >>\nstream\n{stream}\nendstream",
            stream.len()
        ));
        let content_id = objects.len();
        objects.push(format!(
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 3 0 R >> >> /Contents {content_id} 0 R >>"
        ));
        kids.push(format!("{} 0 R", objects.len()));
    }
    objects[0] = "<< /Type /Catalog /Pages 2 0 R >>".to_owned();
    objects[1] = format!(
        "<< /Type /Pages /Kids [{}] /Count {} >>",
        kids.join(" "),
        pages.len()
    );

    let mut out = b"%PDF-1.4\n".to_vec();
    let mut offsets = Vec::with_capacity(objects.len());
    for (index, object) in objects.iter().enumerate() {
        offsets.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n{object}\nendobj\n", index + 1).as_bytes());
    }
    let xref = out.len();
    out.extend_from_slice(
        format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).as_bytes(),
    );
    for offset in offsets {
        out.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
            objects.len() + 1
        )
        .as_bytes(),
    );
    out
}

fn escape(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for c in text.chars() {
        if matches!(c, '\\' | '(' | ')') {
            escaped.push('\\');
        }
        escaped.push(c);
    }
    escaped
}
