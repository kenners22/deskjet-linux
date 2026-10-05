//! A one-page A4 test PDF, written by hand (no Ghostscript needed).

pub fn test_page(stamp: &str) -> Vec<u8> {
    let stamp = stamp.replace(['(', ')', '\\'], "");
    let content = format!(
        "BT /F2 40 Tf 70 720 Td (HP DeskJet 3750) Tj ET\n\
         BT /F1 22 Tf 70 680 Td (Wi-Fi printing OK) Tj ET\n\
         BT /F1 22 Tf 70 650 Td ({stamp}) Tj ET\n\
         0 1 1 rg 70 450 140 140 re f\n\
         1 0 1 rg 230 450 140 140 re f\n\
         1 1 0 rg 390 450 140 140 re f\n\
         0 0 0 rg 70 280 460 140 re f\n"
    );
    let objects = [
        "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string(),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 595 842] /Contents 4 0 R \
         /Resources << /Font << /F1 5 0 R /F2 6 0 R >> >> >>"
            .to_string(),
        format!("<< /Length {} >>\nstream\n{content}endstream", content.len()),
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_string(),
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica-Bold >>".to_string(),
    ];
    let mut out = b"%PDF-1.4\n".to_vec();
    let mut offsets = Vec::new();
    for (i, obj) in objects.iter().enumerate() {
        offsets.push(out.len());
        out.extend(format!("{} 0 obj\n{obj}\nendobj\n", i + 1).bytes());
    }
    let xref = out.len();
    out.extend(format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).bytes());
    for off in offsets {
        out.extend(format!("{off:010} 00000 n \n").bytes());
    }
    out.extend(format!("trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n", objects.len() + 1).bytes());
    out
}
