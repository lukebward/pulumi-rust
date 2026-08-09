[![Deploy](https://get.pulumi.com/new/button.svg)](https://app.pulumi.com/new?template=https://github.com/lukebward/pulumi-rust/tree/main/examples/aws-rs-s3-folder)

# Host a Static Website on Amazon S3

A static website served straight out of an S3 bucket, using
[S3's website support](https://docs.aws.amazon.com/AmazonS3/latest/dev/WebsiteHosting.html).
The program creates a bucket configured to serve `index.html` at the root,
walks the local `www/` directory with `std::fs::read_dir`, and creates one
`aws:s3:BucketObject` per file with a publicly readable ACL and the right
`Content-Type` — the file list is ordinary local data, so it is a plain Rust
`for` loop rather than anything output-shaped. The bucket name and the
website endpoint come back as stack outputs.

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

1.  Set the AWS region:

    ```bash
    $ pulumi config set aws:region us-west-2
    ```

1.  Generate the AWS provider SDK into `./sdks`:

    ```bash
    $ pulumi package gen-sdk aws@7.41.0 --language rust --out ./sdks/aws
    ```

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
    you will be prompted whether to continue.

    ```bash
    $ pulumi up
    Updating (website-testing)

         Type                    Name                              Status
     +   pulumi:pulumi:Stack     aws-rs-s3-folder-website-testing  created
     +   ├─ aws:s3:Bucket        s3-website-bucket                 created
     +   ├─ aws:s3:BucketObject  index.html                        created
     +   └─ aws:s3:BucketObject  styles.css                        created

    Outputs:
        bucket_name: "s3-website-bucket-***"
        website_url: "s3-website-bucket-***.s3-website-us-west-2.amazonaws.com"

    Resources:
        + 4 created

    Duration: ***
    ```

1.  The stack outputs name the bucket and the website endpoint:

    ```bash
    $ pulumi stack output
    Current stack outputs (2):
        OUTPUT       VALUE
        bucket_name  s3-website-bucket-***
        website_url  s3-website-bucket-***.s3-website-us-west-2.amazonaws.com
    ```

1.  Check that both objects landed in the bucket, then fetch the page:

    ```bash
    $ aws s3 ls $(pulumi stack output bucketName)
    2026-08-09 11:02:14        861 index.html
    2026-08-09 11:02:14       1104 styles.css

    $ curl -sS http://$(pulumi stack output websiteUrl) | head -3
    <!doctype html>
    <html lang="en">
      <head>
    ```

    Opening `http://$(pulumi stack output websiteUrl)` in a browser shows
    the styled page.

1.  Edit `www/index.html` and run `pulumi up` again: only the object whose
    contents changed is updated.

1.  Clean up when you are done:

    ```bash
    $ pulumi destroy
    $ pulumi stack rm website-testing
    ```

## A note on public access

The objects are made readable with `acl = "public-read"`, which requires the
bucket to accept public ACLs. AWS accounts created since April 2023 block
them by default (S3 Block Public Access, plus a bucket ownership setting of
`BucketOwnerEnforced`, which disables ACLs outright), and `pulumi up` will
fail with `AccessControlListNotSupported` or `AccessDenied` where that is in
force.

The upstream TypeScript, Python, and Go versions of this example handle that
by adding an `aws:s3:BucketPublicAccessBlock` with `blockPublicAcls = false`,
or by dropping ACLs entirely in favour of an `aws:s3:BucketPolicy` that
grants `s3:GetObject` to everyone. Either is a small addition to
`src/main.rs`.
