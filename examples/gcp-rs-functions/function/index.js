/**
 * An HTTP-triggered Cloud Function that greets the caller.
 *
 * The exported name — `handler` — is what `entry_point` names in
 * `src/main.rs`. Renaming it here means renaming it there too.
 */
exports.handler = (req, res) => {
  const name = (req.query && req.query.name) || (req.body && req.body.name) || "world";
  res.set("Content-Type", "text/plain; charset=utf-8");
  res.status(200).send(`Hello, ${name}! This function was deployed with Pulumi and Rust.\n`);
};
