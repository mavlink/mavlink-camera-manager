# REST API Server Architecture [FULL]

## Overview

Actix-web REST API server providing endpoints for stream management, camera control, and configuration. Includes Swagger UI for API documentation and testing.

## Architecture

### Framework

- **Actix-web**: Web framework with async/await support
- **Swagger UI**: Auto-generated API documentation via `paperclip` crate
- **Async Runtime**: Tokio multi-threaded runtime (10 worker threads)

### Endpoints

Server provides REST endpoints for:
- Stream management (create, list, update, delete)
- Camera control (set controls, get controls)
- Configuration management
- Health checks

### Error Handling

- **Custom Error Types**: Defined in `src/lib/server/error.rs`
- **Error Responses**: Proper HTTP status codes and error messages
- **Validation**: Request validation via `actix-web-validator`

## Key Patterns

### Async Request Handling

All endpoints use async/await patterns. Heavy operations use Tokio spawn for concurrent processing.

### Swagger Integration

API endpoints are annotated with `paperclip` macros for automatic Swagger documentation generation.

### CORS Support

CORS enabled via `actix-cors` for cross-origin requests.

## Common Patterns

### Adding an Endpoint

1. Define handler function (async)
2. Add route in server setup
3. Add Swagger annotations
4. Add request/response types with validation

### Error Responses

Use custom error types that implement `ResponseError` trait for proper HTTP error responses.

## Related Modules

- [Stream](stream.md): Stream manager used by API endpoints
- [Settings](settings.md): Configuration management used by API
- [Controls](../src/lib/controls/): Camera controls exposed via API

## Common Pitfalls

1. **Blocking operations**: Don't block in async handlers - use `spawn_blocking` for CPU-intensive work
2. **Error handling**: Always return proper error types, don't panic in handlers
3. **Validation**: Validate all inputs to prevent invalid state

## Decision Rationale

- **Actix-web**: Mature async web framework for Rust
- **Swagger UI**: Auto-generated docs reduce maintenance burden
- **Async handlers**: Non-blocking request handling for better performance
