"use strict";

// The whole function: API Gateway hands the request over as a payload-format
// 2.0 event, and whatever this returns becomes the HTTP response.
exports.handler = async (event) => {
    const body = {
        message: "Hello, world! Served by AWS Lambda, deployed with Pulumi from Rust.",
        method: event?.requestContext?.http?.method ?? "UNKNOWN",
        path: event?.rawPath ?? "/",
        time: new Date().toISOString(),
    };

    return {
        statusCode: 200,
        headers: { "content-type": "application/json" },
        body: JSON.stringify(body, null, 2),
    };
};
