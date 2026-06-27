use gloo_net::http::Request;
use leptos::*;
use shared::{GpsStatus, Pass, SatImage};

fn main() {
    console_error_panic_hook::set_once();
    mount_to_body(App);
}

// ── API ───────────────────────────────────────────────────────────────────────

async fn fetch_gps() -> GpsStatus {
    match Request::get("/api/gps").send().await {
        Ok(r) => r.json::<GpsStatus>().await.unwrap_or_default(),
        Err(_) => GpsStatus::default(),
    }
}

async fn fetch_passes() -> Vec<Pass> {
    match Request::get("/api/passes").send().await {
        Ok(r) => r.json::<Vec<Pass>>().await.unwrap_or_default(),
        Err(_) => vec![],
    }
}

async fn fetch_images() -> Vec<SatImage> {
    match Request::get("/api/images").send().await {
        Ok(r) => r.json::<Vec<SatImage>>().await.unwrap_or_default(),
        Err(_) => vec![],
    }
}

fn time_until(ts: i64) -> String {
    let now = (js_sys::Date::now() / 1000.0) as i64;
    let diff = ts - now;
    if diff <= 0 {
        return "en cours".to_string();
    }
    let h = diff / 3600;
    let m = (diff % 3600) / 60;
    if h > 0 {
        format!("dans {}h{:02}", h, m)
    } else {
        format!("dans {}min", m)
    }
}

// ── Root ──────────────────────────────────────────────────────────────────────

#[component]
fn App() -> impl IntoView {
    let (tick, set_tick) = create_signal(0u32);
    let gps = create_resource(move || tick.get(), |_| async { fetch_gps().await });
    let passes = create_resource(move || tick.get(), |_| async { fetch_passes().await });
    let images = create_resource(move || tick.get(), |_| async { fetch_images().await });

    view! {
        <div class="min-h-screen bg-slate-900 text-slate-100">

            <header class="bg-slate-800 border-b border-slate-700 sticky top-0 z-40">
                <div class="max-w-7xl mx-auto px-6 py-4 flex items-center justify-between">
                    <div class="flex items-center gap-3">
                        <svg class="w-6 h-6 text-sky-400" fill="none" stroke="currentColor" viewBox="0 0 24 24">
                            <path stroke-linecap="round" stroke-linejoin="round" stroke-width="2"
                                d="M12 2C6.48 2 2 6.48 2 12s4.48 10 10 10 10-4.48 10-10S17.52 2 12 2zm0 18c-4.41 0-8-3.59-8-8s3.59-8 8-8 8 3.59 8 8-3.59 8-8 8zm.5-13H11v6l5.25 3.15.75-1.23-4.5-2.67V7z"/>
                        </svg>
                        <h1 class="text-lg font-bold">
                            <span class="text-sky-400">"Station SDR"</span>
                            <span class="text-slate-500 font-normal ml-2 text-sm">"Météo"</span>
                        </h1>
                    </div>
                    <div class="flex items-center gap-4">
                        <Suspense fallback=|| view! { <span class="text-slate-600 text-xs">"..."</span> }>
                            {move || gps.get().map(|g| view! { <GpsChip gps=g /> })}
                        </Suspense>
                        <button
                            class="px-3 py-1.5 text-xs bg-slate-700 hover:bg-slate-600 rounded-lg transition-colors"
                            on:click=move |_| set_tick.update(|n| *n += 1)
                        >
                            "⟳ Actualiser"
                        </button>
                    </div>
                </div>
            </header>

            <main class="max-w-7xl mx-auto px-4 py-6 space-y-6">

                <div class="grid grid-cols-1 lg:grid-cols-3 gap-4">
                    <Suspense fallback=|| view! {
                        <div class="bg-slate-800 rounded-xl border border-slate-700 h-52 animate-pulse"/>
                    }>
                        {move || passes.get().map(|p| {
                            let next = p.into_iter().next();
                            view! { <NextPassCard pass=next /> }
                        })}
                    </Suspense>

                    <div class="lg:col-span-2">
                        <Suspense fallback=|| view! {
                            <div class="bg-slate-800 rounded-xl border border-slate-700 h-52 animate-pulse"/>
                        }>
                            {move || images.get().map(|imgs| {
                                let latest = imgs.into_iter().next();
                                view! { <LatestImageCard image=latest /> }
                            })}
                        </Suspense>
                    </div>
                </div>

                <section class="bg-slate-800 rounded-xl border border-slate-700 overflow-hidden">
                    <div class="px-6 py-4 border-b border-slate-700 flex items-center justify-between">
                        <h2 class="text-sm font-semibold text-sky-400 uppercase tracking-wider">"Planning 48h"</h2>
                        <Suspense fallback=|| view! { <span/> }>
                            {move || passes.get().map(|p| {
                                let n = p.len();
                                view! { <span class="text-xs text-slate-500">{n}" passages"</span> }
                            })}
                        </Suspense>
                    </div>
                    <Suspense fallback=|| view! {
                        <div class="p-6 text-slate-500 text-sm">"Calcul en cours..."</div>
                    }>
                        {move || passes.get().map(|p| view! { <PassesTable passes=p /> })}
                    </Suspense>
                </section>

                <section class="bg-slate-800 rounded-xl border border-slate-700 overflow-hidden">
                    <div class="px-6 py-4 border-b border-slate-700 flex items-center justify-between">
                        <h2 class="text-sm font-semibold text-sky-400 uppercase tracking-wider">"Archives satellites"</h2>
                        <Suspense fallback=|| view! { <span/> }>
                            {move || images.get().map(|imgs| {
                                let n = imgs.len();
                                view! { <span class="text-xs text-slate-500">{n}" images"</span> }
                            })}
                        </Suspense>
                    </div>
                    <Suspense fallback=|| view! {
                        <div class="p-6 text-slate-500 text-sm">"Chargement..."</div>
                    }>
                        {move || images.get().map(|imgs| view! { <ImagesGallery images=imgs /> })}
                    </Suspense>
                </section>

            </main>

            <footer class="text-center text-xs text-slate-700 pb-6 pt-2">
                "Station SDR — Raspberry Pi 5"
            </footer>
        </div>
    }
}

// ── GPS chip ──────────────────────────────────────────────────────────────────

#[component]
fn GpsChip(gps: GpsStatus) -> impl IntoView {
    let dot = if gps.fix { "w-2 h-2 rounded-full bg-green-500" } else { "w-2 h-2 rounded-full bg-red-500 animate-pulse" };
    let text_class = if gps.fix { "text-green-400 font-mono text-xs" } else { "text-red-400 text-xs" };
    let label = if gps.fix {
        format!("{:.4}°N  {:.4}°E", gps.lat, gps.lon)
    } else {
        "GPS: pas de fix".to_string()
    };
    view! {
        <div class="flex items-center gap-2">
            <span class=dot></span>
            <span class=text_class>{label}</span>
        </div>
    }
}

// ── Next pass card ────────────────────────────────────────────────────────────

#[component]
fn NextPassCard(pass: Option<Pass>) -> impl IntoView {
    let content = match pass {
        None => view! {
            <div class="flex flex-col items-center justify-center flex-1 text-slate-500 gap-2 py-6">
                <svg class="w-10 h-10 opacity-20" fill="none" stroke="currentColor" viewBox="0 0 24 24">
                    <circle cx="12" cy="12" r="10" stroke-width="1.5"/>
                </svg>
                <span class="text-sm">"Aucun passage prévu"</span>
            </div>
        }.into_view(),
        Some(p) => {
            let until = time_until(p.aos_ts);
            let el_color = if p.max_el >= 40.0 { "text-green-400" }
                else if p.max_el >= 20.0 { "text-yellow-400" }
                else { "text-orange-400" };
            view! {
                <div class="space-y-4">
                    <div>
                        <div class="text-xl font-bold text-white">{p.name}</div>
                        <div class="text-sky-400 font-semibold mt-0.5">{until}</div>
                    </div>
                    <div class="grid grid-cols-2 gap-x-4 gap-y-2 text-sm">
                        <span class="text-slate-400 text-xs">"Heure AOS"</span>
                        <span class="font-mono text-right text-sm">{p.aos_fmt}</span>

                        <span class="text-slate-400 text-xs">"Élév. max"</span>
                        <span class=format!("font-mono text-right text-sm {}", el_color)>
                            {format!("{:.0}°", p.max_el)}
                        </span>

                        <span class="text-slate-400 text-xs">"Durée"</span>
                        <span class="font-mono text-right text-sm">{format!("{:.0} min", p.duration_min)}</span>

                        <span class="text-slate-400 text-xs">"Fréquence"</span>
                        <span class="font-mono text-right text-sm text-slate-300">
                            {format!("{:.3} MHz", p.freq_mhz)}
                        </span>
                    </div>
                </div>
            }.into_view()
        }
    };

    view! {
        <div class="bg-slate-800 rounded-xl border border-slate-700 p-6">
            <div class="text-xs font-semibold text-slate-400 uppercase tracking-wider mb-4">
                "Prochain passage"
            </div>
            {content}
        </div>
    }
}

// ── Latest image card ─────────────────────────────────────────────────────────

#[component]
fn LatestImageCard(image: Option<SatImage>) -> impl IntoView {
    let body = match image {
        None => view! {
            <div class="flex flex-col items-center justify-center py-14 text-center gap-3">
                <svg class="w-14 h-14 text-slate-700" fill="none" stroke="currentColor" viewBox="0 0 24 24">
                    <path stroke-linecap="round" stroke-linejoin="round" stroke-width="1"
                        d="M4 16l4.586-4.586a2 2 0 012.828 0L16 16m-2-2l1.586-1.586a2 2 0 012.828 0L20 14m-6-6h.01M6 20h12a2 2 0 002-2V6a2 2 0 00-2-2H6a2 2 0 00-2 2v12a2 2 0 002 2z"/>
                </svg>
                <div>
                    <p class="text-slate-400 text-sm font-medium">"Aucune image disponible"</p>
                    <p class="text-slate-600 text-xs mt-1">"Lancez : sdr auto"</p>
                </div>
            </div>
        }.into_view(),
        Some(img) => view! {
            <div>
                <img src=img.url alt=img.filename
                    class="w-full object-contain max-h-72 bg-black" />
                <div class="px-6 py-3 flex flex-wrap gap-x-3 gap-y-1 text-sm border-t border-slate-700">
                    <span class="font-semibold">{img.satellite}</span>
                    <span class="text-slate-600">"·"</span>
                    <span class="text-slate-400 text-xs self-center">{img.captured_fmt}</span>
                    <span class="text-slate-600">"·"</span>
                    <span class="text-slate-500 text-xs self-center">{format!("Él. {:.0}°", img.elevation)}</span>
                </div>
            </div>
        }.into_view(),
    };

    view! {
        <div class="bg-slate-800 rounded-xl border border-slate-700 overflow-hidden h-full">
            <div class="px-6 pt-5 pb-3 text-xs font-semibold text-slate-400 uppercase tracking-wider">
                "Dernière image satellite"
            </div>
            {body}
        </div>
    }
}

// ── Passes table ──────────────────────────────────────────────────────────────

#[component]
fn PassesTable(passes: Vec<Pass>) -> impl IntoView {
    if passes.is_empty() {
        return view! {
            <div class="p-8 text-center">
                <p class="text-slate-500 text-sm">"Aucun passage calculé"</p>
                <p class="text-slate-600 text-xs mt-1">"Vérifiez que les TLE sont présents dans tle_cache/"</p>
            </div>
        }.into_view();
    }

    let rows = passes
        .into_iter()
        .enumerate()
        .map(|(i, p)| {
            let row_bg = if i % 2 == 0 {
                "border-t border-slate-700/40 hover:bg-slate-700/20 transition-colors"
            } else {
                "border-t border-slate-700/40 bg-slate-900/20 hover:bg-slate-700/20 transition-colors"
            };
            let el_class = if p.max_el >= 40.0 {
                "text-green-400 font-semibold"
            } else if p.max_el >= 20.0 {
                "text-yellow-400"
            } else {
                "text-orange-400"
            };
            let badge = if p.name.starts_with("NOAA") {
                "inline-flex items-center px-2 py-0.5 rounded text-xs font-medium bg-blue-900/60 text-blue-300"
            } else {
                "inline-flex items-center px-2 py-0.5 rounded text-xs font-medium bg-purple-900/60 text-purple-300"
            };
            let el_fmt = format!("{:.0}°", p.max_el);
            let dur_fmt = format!("{:.0}mn", p.duration_min);
            let freq_fmt = format!("{:.3}", p.freq_mhz);
            view! {
                <tr class=row_bg>
                    <td class="px-4 py-3"><span class=badge>{p.name}</span></td>
                    <td class="px-4 py-3 font-mono text-slate-200 text-sm">{p.aos_fmt}</td>
                    <td class="px-4 py-3 text-right font-mono text-sm">
                        <span class=el_class>{el_fmt}</span>
                    </td>
                    <td class="px-4 py-3 text-right font-mono text-slate-300 text-sm">{dur_fmt}</td>
                    <td class="px-4 py-3 text-right font-mono text-slate-400 text-sm">{freq_fmt}</td>
                </tr>
            }
            .into_view()
        })
        .collect::<Vec<_>>();

    view! {
        <div class="overflow-x-auto">
            <table class="w-full">
                <thead>
                    <tr class="text-left text-xs text-slate-500 uppercase tracking-wider bg-slate-900/40">
                        <th class="px-4 py-2.5 font-medium">"Satellite"</th>
                        <th class="px-4 py-2.5 font-medium">"Heure AOS"</th>
                        <th class="px-4 py-2.5 font-medium text-right">"Élév."</th>
                        <th class="px-4 py-2.5 font-medium text-right">"Durée"</th>
                        <th class="px-4 py-2.5 font-medium text-right">"MHz"</th>
                    </tr>
                </thead>
                <tbody>{rows}</tbody>
            </table>
        </div>
    }
    .into_view()
}

// ── Images gallery ────────────────────────────────────────────────────────────

#[component]
fn ImagesGallery(images: Vec<SatImage>) -> impl IntoView {
    if images.is_empty() {
        return view! {
            <div class="p-8 text-center">
                <p class="text-slate-500 text-sm">"Aucune image archivée"</p>
                <p class="text-slate-600 text-xs mt-1">
                    "Les captures apparaissent ici après chaque passage satellite"
                </p>
            </div>
        }
        .into_view();
    }

    let (selected, set_selected) = create_signal::<Option<SatImage>>(None);

    let cards = images
        .into_iter()
        .map(|img| {
            let img_open = img.clone();
            view! {
                <div
                    class="group bg-slate-900 rounded-lg overflow-hidden border border-slate-700 \
                           hover:border-sky-500/50 transition-all cursor-pointer"
                    on:click=move |_| set_selected.set(Some(img_open.clone()))
                >
                    <div class="aspect-video bg-black overflow-hidden">
                        <img
                            src=img.url.clone()
                            alt=img.filename.clone()
                            class="w-full h-full object-cover group-hover:opacity-90 \
                                   group-hover:scale-[1.02] transition-all duration-300"
                            loading="lazy"
                        />
                    </div>
                    <div class="p-2.5">
                        <div class="text-xs font-medium text-slate-200 truncate">{img.satellite}</div>
                        <div class="text-xs text-slate-500 mt-0.5">{img.captured_fmt}</div>
                    </div>
                </div>
            }
            .into_view()
        })
        .collect::<Vec<_>>();

    view! {
        <div>
            // Lightbox
            {move || {
                selected.get().map(|img| {
                    view! {
                        <div
                            class="fixed inset-0 bg-black/90 z-50 flex items-center justify-center p-4 backdrop-blur-sm"
                            on:click=move |_| set_selected.set(None)
                        >
                            <div
                                class="max-w-5xl w-full space-y-3"
                                on:click=|e| e.stop_propagation()
                            >
                                <img
                                    src=img.url.clone()
                                    alt=img.filename.clone()
                                    class="w-full rounded-xl shadow-2xl"
                                />
                                <div class="flex items-center justify-between">
                                    <div class="flex items-center gap-3 text-sm">
                                        <span class="font-semibold text-white">{img.satellite}</span>
                                        <span class="text-slate-400">{img.captured_fmt}</span>
                                        <span class="text-slate-600">{format!("Él. {:.0}°", img.elevation)}</span>
                                    </div>
                                    <button
                                        class="px-4 py-2 bg-slate-800 hover:bg-slate-700 \
                                               rounded-lg text-slate-300 text-sm transition-colors"
                                        on:click=move |_| set_selected.set(None)
                                    >
                                        "✕ Fermer"
                                    </button>
                                </div>
                            </div>
                        </div>
                    }
                })
            }}

            // Grid
            <div class="p-4 grid grid-cols-2 sm:grid-cols-3 lg:grid-cols-4 xl:grid-cols-5 gap-3">
                {cards}
            </div>
        </div>
    }
    .into_view()
}
