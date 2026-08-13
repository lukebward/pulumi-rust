// Copyright 2026, Pulumi Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

package codegen

import (
	"strings"
	"testing"
)

// Every case here is either a real provider property name or the shortest
// name exhibiting a shape that appears in one. The comment says why the case
// exists, because most of them look arbitrary until you know which schema
// they came from.
func TestSnakeCaseWordBreaking(t *testing.T) {
	cases := []struct{ in, want, why string }{
		// A capital after a digit opens a word. The old rule did not break
		// here, which is what produced `ipv4address` and `ipv6cidr_blocks`.
		{"ipv4Address", "ipv4_address", "aws, digitalocean"},
		{"ipv6CidrBlocks", "ipv6_cidr_blocks", "aws"},
		{"s3OriginConfig", "s3_origin_config", "aws cloudfront"},
		{"error404Document", "error404_document", "azure static website"},
		{"enableNfsV3AllSquash", "enable_nfs_v3_all_squash", "azure storage"},

		// A run of capitals is one word, but a lower-case letter ending the
		// run means the run's last capital opens the next word.
		{"HTTPServer", "http_server", "the canonical case"},
		{"publicIPAllocationMethod", "public_ip_allocation_method", "azure network"},
		{"podCIDRSet", "pod_cidr_set", "kubernetes"},
		{"IPSet", "ip_set", "shortest run-end case"},
		{"JSONPath", "json_path", "leading acronym run"},
		{"allowVNetOverride", "allow_v_net_override", "two-letter run, ended by a lower-case letter"},

		// A run reaching the end of the name stays whole.
		{"parseJSON", "parse_json", "trailing acronym"},
		{"ACLs", "acls", "the whole name is a plural acronym"},

		// Digits fold into an acronym rather than splitting it.
		{"SHA256Hash", "sha256_hash", "digit inside a run"},
		{"Sha256Hash", "sha256_hash", "same result via the lower/digit path"},
		{"HTTP2Server", "http2_server", "digit absorbed, then digit-then-capital"},
		{"openAPIV3Schema", "open_apiv3_schema", "kubernetes CRD schema"},

		// A trailing "s" that closes a run of capitals belongs to the
		// acronym, so a plural acronym is not shredded.
		{"podCIDRs", "pod_cidrs", "kubernetes"},
		{"podIPs", "pod_ips", "kubernetes"},
		{"nonResourceURLs", "non_resource_urls", "kubernetes rbac"},
		{"someTHINGsAREWeird", "some_things_are_weird", "the 's' is followed by a capital, so it stays in the run"},

		// ...but only when the "s" is not itself starting a word. Python
		// folds it unconditionally and has to hard-code its way out of the
		// fallout (pulumi/pulumi#5199); the lookahead gets it right.
		{"openXJsonSerDe", "open_x_json_ser_de", "aws glue; the case Python special-cases"},
		{"sendMDNAsynchronously", "send_mdn_asynchronously", "azure-native; same shape"},

		// A single lower-case letter wedged between a run of capitals and a
		// digit is part of the acronym: a version suffix, not a new word.
		{"isIPv6Enabled", "is_ipv6_enabled", "azure sql; Python gives is_i_pv6_enabled"},
		{"isNFSv3Enabled", "is_nfsv3_enabled", "azure storage"},
		{"privateIPv4Address", "private_ipv4_address", "azure network"},

		// Names that are simply ugly in the schema stay legible-ish.
		{"iPAddressOrRange", "i_p_address_or_range", "azure-native"},

		// Shapes the old rule already handled, kept working.
		{"kubeletConfigKey", "kubelet_config_key", "ordinary camelCase"},
		{"$ref", "ref", "kubernetes CRD; non-identifier runes are dropped, not separated"},
		{"$schema", "schema", "kubernetes CRD"},
		{"1abc", "_1abc", "a Rust identifier cannot start with a digit"},
		{"some-name", "some_name", "kebab-case"},
		{"some__name", "some_name", "a run of separators collapses"},
		{"", "_", "an empty name still has to be an identifier"},
	}

	for _, c := range cases {
		if got := snakeCase(c.in); got != c.want {
			t.Errorf("snakeCase(%q) = %q, want %q (%s)", c.in, got, c.want, c.why)
		}
	}
}

// The new boundaries are a superset of the old ones: the rule only ever
// inserts underscores, never moves or removes a letter. That is what makes
// the change incapable of merging two distinct property names onto one
// identifier.
func TestSnakeCaseOnlyInsertsSeparators(t *testing.T) {
	for _, name := range []string{
		"ipv4Address", "publicIPAllocationMethod", "podCIDRs", "openAPIV3Schema",
		"isIPv6Enabled", "parseJSON", "$ref", "some-name", "HTTP2Server",
	} {
		got := snakeCase(name)
		var strippedB strings.Builder
		for _, r := range got {
			if r != '_' {
				strippedB.WriteRune(r)
			}
		}
		stripped := strippedB.String()
		var lettersB strings.Builder
		for _, r := range name {
			if isNameSeparator(r) {
				continue
			}
			if r >= 'A' && r <= 'Z' {
				lettersB.WriteRune(r + 32)
			} else if (r >= 'a' && r <= 'z') || (r >= '0' && r <= '9') {
				lettersB.WriteRune(r)
			}
		}
		lettersOnly := lettersB.String()
		if stripped != lettersOnly {
			t.Errorf("snakeCase(%q) = %q: letters changed (%q vs %q)",
				name, got, stripped, lettersOnly)
		}
	}
}
