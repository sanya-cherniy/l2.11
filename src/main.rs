use axum::{
    extract::{Json, Query, State},
    http::{Method, StatusCode, Uri},
    middleware,
    response::IntoResponse,
    response::Response,
    routing::{get, post},
    Router,
};
use std::{
    error::Error,
    net::{IpAddr, SocketAddr},
    str::FromStr,
    sync::{Arc, Mutex},
    time::{SystemTime, UNIX_EPOCH},
};

use chrono::{DateTime, Datelike, Duration, NaiveDate, Utc};
use config::{Config, File};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    // Создаем новый конфиг
    let config = Config::builder()
        .set_default("address", "127.0.0.1")? // Устанавливаем значение по умолчанию
        .set_default("port", 8080)? // Устанавливаем значение по умолчанию
        .add_source(File::with_name("config")) // Указываем путь к файлу конфигурации
        .build()?; // Создаем конфигурацию

    // Извлекаем настройки
    let settings: Settings = config.try_deserialize()?;

    // Используем настройки
    let ip: IpAddr = settings.address.parse()?;
    let addr: SocketAddr = SocketAddr::new(ip, settings.port);
    // Здесь храним даты и события
    let dates: Arc<Mutex<Vec<Event>>> = Arc::new(Mutex::new(Vec::new()));
    // Создаем роутеры
    let app = Router::new()
        .route("/create_event", post(create_event_handler))
        .route("/update_event", post(update_event_handler))
        .route("/delete_event", post(delete_event_handler))
        .route("/events_for_day", get(events_for_day_handler))
        .route("/events_for_week", get(events_for_week_handler))
        .route("/events_for_month", get(events_for_month_handler))
        .with_state(dates)
        .layer(middleware::map_response(log_request));
    println!("LISTENING on {addr}\n");
    // Запускаем сервер
    axum::Server::bind(&addr)
        .serve(app.into_make_service())
        .await?;
    Ok(())
}

// Функция для логирования через middleware
pub async fn log_request(req_method: Method, uri: Uri, res: Response) -> Response {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis();

    let log_line = RequestLogLine {
        status: res.status().to_string(),
        timestamp: timestamp.to_string(),
        req_path: uri.to_string(),
        req_method: req_method.to_string(),
    };

    println!("   ->> log_request: \n{}", json!(log_line));
    res
}

#[derive(Serialize)]
struct RequestLogLine {
    status: String,
    timestamp: String,
    req_path: String,
    req_method: String,
}

// Обработчик создания события
async fn create_event_handler(
    State(dates): State<Arc<Mutex<Vec<Event>>>>,
    Json(body): Json<Value>,
) -> Response {
    // Проверяем на валидность входные данные
    let event = match json_body_parse(body).await {
        Ok(value) => value,
        Err(e) => {
            return e;
        }
    };
    // Проверяем что указанное событие не было добавлено ранее
    if let Some(_) = check_event(&dates, &event).await {
        let res = json!({
            "error": format!("Data already exist")
        });
        return (axum::http::StatusCode::SERVICE_UNAVAILABLE, Json(res)).into_response();
    } else {
        let dates = dates.lock();
        match dates {
            Ok(mut dates) => {
                let res = json!({
                    "result": format!("Added event: '{}' for date {}", event.name, event.date),
                });
                // Сохраняем полученные данные
                dates.push(event);
                return (StatusCode::CREATED, Json(res)).into_response();
            }
            Err(_) => {
                return (axum::http::StatusCode::INTERNAL_SERVER_ERROR).into_response();
            }
        }
    }
}

// Функция для обновления данных о событии
async fn update_event_handler(
    State(dates): State<Arc<Mutex<Vec<Event>>>>,
    Json(body): Json<Value>,
) -> Response {
    // Десериализация данных
    let body: Result<EventUpdateReq, _> = serde_json::from_value(body);

    match body {
        Ok(body) => {
            // Успешная десериализация
            let date = match DateTime::parse_from_rfc3339(&body.date_time) {
                Ok(value) => value,
                Err(e) => {
                    let res = json!({
                        "error": format!("{}",e),
                    });
                    return (StatusCode::BAD_REQUEST, Json(res)).into_response();
                }
            };
            let event = Event {
                date: date.with_timezone(&Utc),
                name: body.event_name.clone(),
            };
            // Проверяем что указанное событие пристутствует в памяти
            if let Some(i) = check_event(&dates, &event).await {
                let date = match DateTime::parse_from_rfc3339(&body.new_date_time) {
                    Ok(value) => value,
                    Err(e) => {
                        let res = json!({
                            "error": format!("{}",e),
                        });
                        return (StatusCode::BAD_REQUEST, Json(res)).into_response();
                    }
                };
                let dates = dates.lock();
                match dates {
                    // Изменяем данные
                    Ok(mut dates) => {
                        dates[i].date = date.with_timezone(&Utc);
                        dates[i].name = body.new_event_name.clone();
                        let res = json!({
                            "result": format!("Update event: '{}' for date {}, on event: '{}' for date {}", body.event_name,body.date_time,body.new_event_name,body.new_date_time),
                        });
                        return (StatusCode::OK, Json(res)).into_response();
                    }
                    Err(_) => {
                        return (axum::http::StatusCode::INTERNAL_SERVER_ERROR).into_response();
                    }
                }
            } else {
                let res = json!({
                    "error": format!("The data does not exist"),
                });
                return (axum::http::StatusCode::SERVICE_UNAVAILABLE, Json(res)).into_response();
            }
        }
        Err(e) => {
            let res = json!({
                "error": format!("{}",e),
            });
            return (StatusCode::BAD_REQUEST, Json(res)).into_response();
            // Ошибка десериализации
        }
    }
}

// Обработчик для удаления событий
async fn delete_event_handler(
    State(dates): State<Arc<Mutex<Vec<Event>>>>,
    Json(body): Json<Value>,
) -> Response {
    // Проверяем на валидность входные данные
    let event = match json_body_parse(body).await {
        Ok(value) => value,
        Err(e) => {
            return e;
        }
    };
    // Проверяем что указанное событие не было добавлено ранее
    if let Some(i) = check_event(&dates, &event).await {
        let dates = dates.lock();
        match dates {
            Ok(mut dates) => {
                let res = json!({
                    "result": format!("Removed event: '{}' for date {}",event.name,event.date),
                });
                // Удаляем найденное событие
                dates.remove(i);
                return (StatusCode::OK, Json(res)).into_response();
            }
            Err(_) => {
                return (axum::http::StatusCode::INTERNAL_SERVER_ERROR).into_response();
            }
        }
    }
    // Если указанное событие не было найдено - возвращаем  HTTP 503s
    else {
        let res = json!({
            "error": format!("The data does not exist"),
        });
        return (axum::http::StatusCode::SERVICE_UNAVAILABLE, Json(res)).into_response();
    }
}

// Обработчик, возващающий все события дня для указанной даты
async fn events_for_day_handler(
    State(dates): State<Arc<Mutex<Vec<Event>>>>,
    Query(param): Query<Value>,
) -> Response {
    // Проверяем на валидность входные данные
    let desired_date = match query_parse(param).await {
        Ok(value) => value,
        Err(e) => {
            return e;
        }
    };
    let dates = dates.lock();
    match dates {
        Ok(dates) => {
            // Проходим по всем имеющимся событиям и оставляем те, день, месяц и год которых соответствуют указанному событию
            let filtered_dates: Vec<&Event> = dates
                .iter()
                .filter(|event| {
                    event.date.year() == desired_date.year()
                        && event.date.month() == desired_date.month()
                        && event.date.day() == desired_date.day()
                })
                .collect();

            let res = json!({
                "result": filtered_dates,
            });

            return (StatusCode::OK, Json(res)).into_response();
        }
        Err(_) => {
            return (StatusCode::INTERNAL_SERVER_ERROR).into_response();
        }
    }
}

// Обработчик, возващающий все события недели для указанной даты
async fn events_for_week_handler(
    State(dates): State<Arc<Mutex<Vec<Event>>>>,
    Query(param): Query<Value>,
) -> Response {
    // Проверяем на валидность входные данные
    let desired_date = match query_parse(param).await {
        Ok(value) => value,
        Err(e) => {
            return e;
        }
    };
    let dates = dates.lock();
    match dates {
        Ok(dates) => {
            // Проходим по всем имеющимся событиям и оставляем те, начала недели у кооторых совпадают с указанным событием
            let filtered_dates: Vec<&Event> = dates
                .iter()
                .filter(|event| {
                    let start_week_1 = start_of_week(event.date.date_naive());
                    let start_week_2 = start_of_week(desired_date);
                    start_week_1 == start_week_2
                })
                .collect();

            let res = json!({
                "result": filtered_dates,
            });

            return (StatusCode::OK, Json(res)).into_response();
        }
        Err(_) => {
            return (StatusCode::INTERNAL_SERVER_ERROR).into_response();
        }
    }
}

// Обработчик, возващающий все события месяца для указанной даты
async fn events_for_month_handler(
    State(dates): State<Arc<Mutex<Vec<Event>>>>,
    Query(param): Query<Value>,
) -> Response {
    let desired_date = match query_parse(param).await {
        Ok(value) => value,
        Err(e) => {
            return e;
        }
    };
    let dates = dates.lock();
    match dates {
        Ok(dates) => {
            // Проходим по всем имеющимся событиям и оставляем те, месяц и год которых соответствуют указанному событию
            let filtered_dates: Vec<&Event> = dates
                .iter()
                .filter(|event| {
                    event.date.year() == desired_date.year()
                        && event.date.month() == desired_date.month()
                })
                .collect();

            let res = json!({
                "result": filtered_dates,
            });

            return (StatusCode::OK, Json(res)).into_response();
        }
        Err(_) => {
            return (StatusCode::INTERNAL_SERVER_ERROR).into_response();
        }
    }
}
// Функция для определения начала недели для указанной даты
fn start_of_week(date: NaiveDate) -> NaiveDate {
    let diff = date.weekday().num_days_from_monday();
    date - Duration::days(diff as i64)
}
// Функция для нахождения указанного события в массиве событий
async fn check_event(events: &Arc<Mutex<Vec<Event>>>, desired_event: &Event) -> Option<usize> {
    let events = events.lock().unwrap();
    for (i, event) in events.iter().enumerate() {
        if event.date == desired_event.date && event.name == desired_event.name {
            return Some(i);
        }
    }
    return None;
}

// Функция для извлечения даты из query-строки
async fn query_parse(param: Value) -> Result<NaiveDate, Response> {
    let query: DateParam = match serde_json::from_value(param) {
        Ok(query) => query,
        Err(e) => {
            let res = json!({
                "error": format!("{}",e),
            });
            return Err((StatusCode::BAD_REQUEST, Json(res)).into_response());
        }
    };
    match NaiveDate::from_str(&query.date) {
        Ok(value) => {
            return Ok(value);
        }
        Err(e) => {
            let res = json!({
                "error": format!("{}",e),
            });
            return Err((StatusCode::BAD_REQUEST, Json(res)).into_response());
        }
    };
}
// Функция для извлечения даты и названия события из json
async fn json_body_parse(body: Value) -> Result<Event, Response> {
    let body: Result<EventReq, _> = serde_json::from_value(body);
    match body {
        Ok(body) => {
            match DateTime::parse_from_rfc3339(&body.date_time) {
                Ok(value) => {
                    return Ok(Event {
                        date: value.with_timezone(&Utc),
                        name: body.event_name,
                    })
                }
                Err(e) => {
                    let res = json!({
                        "error": format!("{}",e),
                    });
                    return Err((StatusCode::BAD_REQUEST, Json(res)).into_response());
                }
            };
        }
        Err(e) => {
            let res = json!({
                "error": format!("{}",e),
            });
            return Err((StatusCode::BAD_REQUEST, Json(res)).into_response());
        }
    }
}
#[derive(Deserialize)]
struct DateParam {
    date: String,
}

#[derive(Deserialize)]
struct EventReq {
    date_time: String,
    event_name: String,
}

#[derive(Deserialize)]
struct EventUpdateReq {
    date_time: String,
    event_name: String,
    new_date_time: String,
    new_event_name: String,
}

#[derive(Deserialize, Serialize)]
struct Event {
    date: DateTime<Utc>,
    name: String,
}

#[derive(Debug, Deserialize)]
struct Settings {
    address: String,
    port: u16,
}
