/**
 * An anonymous HTTP-triggered Azure Function that greets the caller.
 *
 * The directory name — `HelloNode` — is the function's name, and so the
 * route the host serves it on: `/api/HelloNode`. Renaming the directory
 * changes the `endpoint` stack output in `src/main.rs` too.
 */
module.exports = async function (context, req) {
    const name = (req.query && req.query.name) || (req.body && req.body.name) || "world";
    context.res = {
        status: 200,
        headers: { "Content-Type": "text/plain; charset=utf-8" },
        body: `Hello, ${name}! This function was deployed with Pulumi and Rust.\n`,
    };
};
