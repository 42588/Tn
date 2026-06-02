use pdfium_render::prelude::*;

fn main() {
    let pdfium_dll = "d:\\coder\\Tn\\pdfium.dll";
    match Pdfium::bind_to_system_library().or_else(|_| Pdfium::bind_to_library(pdfium_dll)) {
        Ok(_) => println!("Pdfium bind OK"),
        Err(e) => println!("Pdfium bind Error: {:?}", e),
    }
}
