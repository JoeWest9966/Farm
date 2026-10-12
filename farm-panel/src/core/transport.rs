//! 浏览器传输层（ticket #22 起，#25 扩展）：把数据核拼好的 [`HttpRequest`] 经
//! `fetch` 发出去，把字节交回来成 [`HttpResponse`]——**自己不拼 URL、不带 token**
//! （ADR-0002 §1：请求由数据核拼，传输层只搬运）。只在 wasm 侧存在。

use super::client::{HttpRequest, HttpResponse};

/// 经 `fetch` 发一个请求，回原始字节（传输失败 → `Err`）。
///
/// 数据核已经带了 token header + JSON 体 + 完整路径；这里只做搬运 + 读字节。
#[cfg(target_arch = "wasm32")]
pub async fn send(base_url: &str, request: &HttpRequest) -> Result<HttpResponse, String> {
    use wasm_bindgen::JsCast;
    use wasm_bindgen_futures::JsFuture;
    use web_sys::{Headers, Request, RequestInit, Response};

    let url = request.url(base_url);

    let opts = RequestInit::new();
    opts.set_method(request.method);
    opts.set_body(&wasm_bindgen::JsValue::from_str(request.body.as_deref().unwrap_or("")));

    let headers = Headers::new().map_err(|e| format!("headers: {e:?}"))?;
    for (k, v) in &request.headers {
        headers
            .set(k, v)
            .map_err(|e| format!("set header {k}: {e:?}"))?;
    }
    opts.set_headers(&headers.into());

    let request = Request::new_with_str_and_init(&url, &opts).map_err(|e| format!("request: {e:?}"))?;
    let window = web_sys::window().ok_or_else(|| "no window".to_string())?;
    let resp_value = JsFuture::from(window.fetch_with_request(&request))
        .await
        .map_err(|e| format!("fetch: {e:?}"))?;
    let resp: Response = resp_value.dyn_into().map_err(|_| "response cast".to_string())?;
    let status = resp.status();

    let body = JsFuture::from(resp.array_buffer().map_err(|e| format!("array_buffer: {e:?}"))?)
        .await
        .map_err(|e| format!("read body: {e:?}"))?;
    let body = js_sys::Uint8Array::new(&body).to_vec();

    Ok(HttpResponse { status, body })
}
