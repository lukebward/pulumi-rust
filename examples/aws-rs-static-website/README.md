[![Deploy](https://get.pulumi.com/new/button.svg)](https://app.pulumi.com/new?template=https://github.com/pulumi-labs/pulumi-rust/tree/main/examples/aws-rs-static-website)

# Static Website on Amazon S3 and CloudFront

A static website served from the CloudFront CDN, over HTTPS, out of a
bucket that is not public.

The program walks the local `www/` directory with `std::fs::read_dir` and
creates one `aws:s3:BucketObject` per file — the file list is ordinary local
data, so it is a plain Rust `for` loop rather than anything output-shaped —
then puts a `aws:cloudfront:Distribution` in front of the bucket. Access is
granted with an
[origin access identity](https://docs.aws.amazon.com/AmazonCloudFront/latest/DeveloperGuide/private-content-restricting-access-to-s3.html):
the CDN signs its requests to S3 as that principal, and a bucket policy
grants `s3:GetObject` to it and to nobody else. The bucket's name and the
CDN's URL come back as stack outputs.

This is the companion to [`aws-rs-s3-folder`](../aws-rs-s3-folder), which
serves the same kind of site straight off S3's website endpoint: public
objects, HTTP only, one region. This one is private, HTTPS, and cached at
the edge.

## Prerequisites

1. [Install Pulumi](https://www.pulumi.com/docs/install/).
2. [Install Rust](https://rustup.rs/) (1.85 or newer) — `cargo` builds the
   program.
3. Build the experimental Rust language plugin from this repository and put
   it on your `PATH`, so that `runtime: rust` resolves:

   ```bash
   $ (cd ../../pulumi-language-rust && go build .)
   $ export PATH="$(cd ../../pulumi-language-rust && pwd):$PATH"
   ```

4. [Configure AWS credentials](https://www.pulumi.com/registry/packages/aws/installation-configuration/),
   for example by setting `AWS_PROFILE` or running `aws configure`.

**The AWS SDK is not checked in.** `Cargo.toml` points `pulumi_aws` at
`./sdks/aws/rust`, which does not exist until you run the `pulumi package gen-sdk`
command in step 3 below. The crate does not build before then.

## Deploying and running the program

Note: some values in this example will be different from run to run. These
values are indicated with `***`.

1.  Create a new stack:

    ```bash
    $ pulumi stack init website-testing
    ```

1.  Set the AWS region. The bucket lives there; the CDN is global either
    way:

    ```bash
    $ pulumi config set aws:region us-west-2
    ```

1.  Generate the AWS provider SDK into `./sdks`:

    ```bash
    $ pulumi package gen-sdk aws@7.41.0 --language rust --out ./sdks/aws
    ```

    Note that `gen-sdk` writes to `<out>/<language>`, so the crate lands in
    `./sdks/aws/rust` — which is the path `Cargo.toml` already points at.
    The generated crate's own `Cargo.toml` depends on `pulumi = "0.1"`,
    which is not published yet; repoint it at this repository:

    ```toml
    # in ./sdks/aws/rust/Cargo.toml
    pulumi = { path = "../../../../../sdk/rust/pulumi" }
    ```

    The version is pinned because the property names in `src/main.rs` were
    checked against that schema. Every generated args struct derives
    `Default`, so a provider version that adds an optional input will not
    break this program; one that renames or removes an input still will.

1.  Run `pulumi up` to preview and deploy changes. After the preview is shown
    you will be prompted whether to continue. CloudFront takes several
    minutes to roll a new distribution out to its edge locations, and
    `pulumi up` waits for it.

    ```bash
    $ pulumi up
    Updating (website-testing)

         Type                                Name                            Status
     +   pulumi:pulumi:Stack                 aws-rs-static-website-***       created
     +   ├─ aws:s3:Bucket                    content-bucket                  created
     +   ├─ aws:s3:BucketObject              index.html                      created
     +   ├─ aws:cloudfront:OriginAccessIdentity  content-origin-access-identity  created
     +   ├─ aws:s3:BucketPolicy              content-bucket-policy           created
     +   └─ aws:cloudfront:Distribution      cdn                             created

    Outputs:
        bucketName:     "content-bucket-***"
        cdnUrl:         "https://***.cloudfront.net"
        distributionId: "***"

    Resources:
        + 5 created

    Duration: ***
    ```

1.  The stack outputs name the bucket and the CDN:

    ```bash
    $ pulumi stack output
    Current stack outputs (3):
        OUTPUT          VALUE
        bucketName      content-bucket-***
        cdnUrl          https://***.cloudfront.net
        distributionId  ***
    ```

1.  Fetch the page through the CDN:

    ```bash
    $ curl -sS $(pulumi stack output cdnUrl) | head -3
    <!doctype html>
    <html lang="en">
      <head>
    ```

    A second request is served from the edge cache — the `X-Cache` response
    header says which:

    ```bash
    $ curl -sSI $(pulumi stack output cdnUrl) | grep -i x-cache
    x-cache: Hit from cloudfront
    ```

1.  Check that the bucket itself is *not* readable. This is the whole point
    of the origin access identity:

    ```bash
    $ curl -sS https://$(pulumi stack output bucketName).s3.amazonaws.com/index.html | head -3
    <?xml version="1.0" encoding="UTF-8"?>
    <Error><Code>AccessDenied</Code>...
    ```

1.  Edit `www/index.html` and run `pulumi up` again: only the object whose
    contents changed is updated. The edge cache still holds the old copy for
    up to the `default_ttl` of ten minutes, so invalidate it to see the
    change straight away:

    ```bash
    $ aws cloudfront create-invalidation \
        --distribution-id $(pulumi stack output distributionId) --paths '/*'
    ```

1.  Clean up when you are done. Disabling and deleting a distribution takes
    a few minutes:

    ```bash
    $ pulumi destroy
    $ pulumi stack rm website-testing
    ```

## Notes

- **The origin is the bucket's REST endpoint**
  (`bucket_regional_domain_name`), not its website endpoint. An origin
  access identity only authenticates against the REST API; the website
  endpoint is public, HTTP-only, and would need a `custom_origin_config`
  instead. The trade-off is that the REST endpoint has no notion of an index
  document, so `default_root_object` covers `/` and requests for a
  subdirectory like `/blog/` return a 403 rather than `/blog/index.html`.
- **The bucket policy is built as a value, not spliced as a string.**
  `pulumi::pv::to_json` over a `pv::object` waits for the identity's ARN,
  and an unknown value inside stays unknown all the way out — so a preview
  shows the policy as unknown rather than as JSON with a hole in it.
- **No ACLs anywhere.** The objects are private and the bucket policy is the
  only grant, which is why this example works unchanged on AWS accounts
  created since April 2023, where S3 Block Public Access and
  `BucketOwnerEnforced` object ownership are on by default. `aws-rs-s3-folder`
  uses `acl = "public-read"` and needs those settings relaxed.
- **`forwarded_values` versus a cache policy.** The cache behaviour here
  uses the older `forwarded_values` block plus explicit TTLs, which keeps
  the program self-contained. The modern equivalent is `cache_policy_id`
  pointing at a managed policy such as `CachingOptimized`; the two are
  mutually exclusive.
- A custom domain would add the name to `aliases` and set
  `acm_certificate_arn` on the viewer certificate — with the certificate
  issued in `us-east-1`, which is the only region CloudFront reads them
  from — plus a Route 53 alias record pointing at the distribution.
