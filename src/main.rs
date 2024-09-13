use esp_idf_sys as _; // Bindings to the ESP-IDF SDK

use anyhow::Result;
use embedded_svc::http::server::{HttpServer, Request, Response};
use embedded_svc::io::Write;
use esp_idf_hal::gpio::{Gpio5, Output, PinDriver};
use esp_idf_hal::prelude::*;
use esp_idf_svc::eventloop::EspSystemEventLoop;
use esp_idf_svc::http::server::EspHttpServer;
use esp_idf_svc::netif::*;
use esp_idf_svc::nvs::EspDefaultNvsPartition;
use esp_idf_svc::wifi::*;
use heapless::spsc::Queue;
use log::*;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};
use dotenv::dotenv;

use embedded_graphics::{
    mono_font::{ascii::FONT_6X10, MonoTextStyle},
    pixelcolor::Rgb565,
    prelude::*,
    text::Text,
};
use st7789::Orientation;

fn main() -> Result<()> {
    dotenv().ok();

    esp_idf_sys::link_patches();
    esp_idf_svc::log::EspLogger::initialize_default();

    // Initialisiere Peripherie
    let peripherals = Peripherals::take().unwrap();
    let pins = peripherals.pins;

    // Initialisiere GPIO für das Relais an Pin 5
    let mut relay = PinDriver::output(pins.gpio5)?;

    // WLAN-Konfiguration aus Umgebungsvariablen
    let ssid = env!("WIFI_SSID");
    let password = env!("WIFI_PASS");

    // WLAN initialisieren und verbinden
    let sys_loop = EspSystemEventLoop::take()?;
    let default_nvs = EspDefaultNvsPartition::take()?;
    let mut wifi = EspWifi::new(peripherals.modem, sys_loop.clone(), Some(default_nvs))?;

    let wifi_config = WifiConfiguration::Client(ClientConfiguration {
        ssid: ssid.into(),
        password: password.into(),
        ..Default::default()
    });

    wifi.set_configuration(&wifi_config)?;
    wifi.start()?;
    info!("WLAN gestartet");
    wifi.connect()?;
    info!("Verbinde mit WLAN...");

    // Warte auf Verbindung
    while !wifi.is_connected().unwrap() {
        std::thread::sleep(Duration::from_millis(500));
    }
    info!("Mit WLAN verbunden");

    // IP-Adresse abrufen
    let ip_info = wifi.sta_netif().get_ip_info()?;
    let ip_address = ip_info.ip.to_string();
    info!("IP-Adresse: {}", ip_address);

    // Initialisiere Display
    // Hier muss die spezifische Initialisierung für das HTIT-WB32 Display erfolgen
    // Zum Beispiel:
    let display = initialize_display(peripherals.spi2, pins.gpio18, pins.gpio23, pins.gpio5)?;

    // Log-Queue für die Anzeige
    let log_queue: Arc<Mutex<Queue<String, 10>>> = Arc::new(Mutex::new(Queue::new()));

    // HTTP-Server konfigurieren
    let server_config = esp_idf_svc::http::server::Configuration::default();
    let mut server = EspHttpServer::new(&server_config)?;

    // Endpunkt /health
    {
        let log_queue = log_queue.clone();
        server.fn_handler("/health", embedded_svc::http::Method::Get, move |req| {
            let response_body = r#"{ "status": "up" }"#;
            let mut resp = req.into_ok_response()?;
            resp.write_all(response_body.as_bytes())?;

            // Loggen
            log_request(&log_queue, 200, "/health");

            Ok(())
        })?;
    }

    // Endpunkt /push
    {
        let log_queue = log_queue.clone();
        let relay = relay.clone();
        server.fn_handler("/push", embedded_svc::http::Method::Get, move |req| {
            // Relais für 500ms schließen
            relay.set_high()?;
            std::thread::sleep(Duration::from_millis(500));
            relay.set_low()?;

            let response_body = r#"{ "success": true }"#;
            let mut resp = req.into_ok_response()?;
            resp.write_all(response_body.as_bytes())?;

            // Loggen
            log_request(&log_queue, 200, "/push");

            Ok(())
        })?;
    }

    // 404 für alle anderen Pfade
    {
        let log_queue = log_queue.clone();
        server.handler(move |req| {
            let path = req.path().to_string();
            let resp = req.into_response(404, None, &[])?;
            log_request(&log_queue, 404, &path);
            Ok(())
        })?;
    }

    // Hauptschleife zur Aktualisierung des Displays
    loop {
        // Display aktualisieren
        update_display(&display, &ip_address, &log_queue)?;

        std::thread::sleep(Duration::from_millis(1000));
    }
}

fn log_request(log_queue: &Arc<Mutex<Queue<String, 10>>>, status: u16, path: &str) {
    let timestamp = SystemTime::now();
    let datetime: chrono::DateTime<chrono::Local> = timestamp.into();
    let log_entry = format!("{} {} {}", datetime.format("%H:%M:%S"), status, path);

    let mut queue = log_queue.lock().unwrap();
    if queue.is_full() {
        queue.dequeue();
    }
    queue.enqueue(log_entry).unwrap();
}

fn initialize_display(
    spi: impl embedded_hal::spi::FullDuplex<u8> + 'static,
    dc: impl OutputPin + 'static,
    rst: impl OutputPin + 'static,
    bl: impl OutputPin + 'static,
) -> Result<st7789::ST7789<SpiInterfaceNoCS<SPI, DC>, RST>> {
    // Display initialisieren (angepasst an das HTIT-WB32 Modell)
    // Dieser Code muss an die spezifische Hardware angepasst werden

    let interface = SPIInterfaceNoCS::new(spi, dc);
    let mut display = st7789::ST7789::new(
        interface,
        rst,
        240, // Breite
        320, // Höhe
    );

    display.init(&mut Delay)?;
    display.set_orientation(Orientation::Portrait)?;

    Ok(display)
}

fn update_display(
    display: &impl DrawTarget<Color = Rgb565>,
    ip_address: &str,
    log_queue: &Arc<Mutex<Queue<String, 10>>>,
) -> Result<()> {
    // Bildschirm löschen
    display.clear(Rgb565::BLACK)?;

    // WLAN-Status und IP-Adresse anzeigen
    let text_style = MonoTextStyle::new(&FONT_6X10, Rgb565::WHITE);
    Text::new(&format!("IP: {}", ip_address), Point::new(0, 10), text_style)
        .draw(display)?;

    // Logs anzeigen
    let queue = log_queue.lock().unwrap();
    let mut y = 30;
    for log_entry in queue.iter() {
        Text::new(log_entry, Point::new(0, y), text_style).draw(display)?;
        y += 12;
    }

    Ok(())
}
