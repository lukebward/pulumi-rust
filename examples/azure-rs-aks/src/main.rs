//! A managed Kubernetes cluster on Azure (AKS).
//!
//! A resource group holds one `containerservice:ManagedCluster` with a
//! single scale-set-backed agent pool. The cluster authenticates to Azure
//! with a **system-assigned managed identity**, so no Entra ID application,
//! service principal or client secret has to be created first — this program
//! needs only the azure-native provider.
//!
//! The cluster's admin-free user kubeconfig is fetched with the
//! `containerservice:listManagedClusterUserCredentials` invoke, base64
//! decoded, and exported as a secret stack output.
//!
//! The program depends on a generated azure-native SDK, so generate that
//! first:
//!
//! ```sh
//! pulumi package add azure-native@3.25.0
//! pulumi config set azure-native:location WestUS
//! pulumi up
//! ```

use pulumi_azure_native::{containerservice, resources, types};

/// The default VM size for the agent pool. Two vCPUs and 7 GiB is the
/// smallest size Azure recommends for a System pool.
const DEFAULT_VM_SIZE: &str = "Standard_DS2_v2";

/// The default number of nodes in the agent pool.
const DEFAULT_NODE_COUNT: f64 = 2.0;

fn main() {
    pulumi::run(|ctx| async move {
        let config = ctx.config();

        // `pulumi config set nodeCount 3` and
        // `pulumi config set nodeVmSize Standard_D4s_v5` to override.
        let node_count = config.get_int_or(
            "nodeCount",
            pulumi::PropertyValue::Number(DEFAULT_NODE_COUNT),
        );
        let node_vm_size = config.get_string_or(
            "nodeVmSize",
            pulumi::PropertyValue::String(DEFAULT_VM_SIZE.into()),
        );

        // The cluster lands in its own resource group. The region comes from
        // the provider's `azure-native:location` config rather than being
        // hard-coded here.
        //
        // Every input of `ResourceGroupArgs` is optional in azure-native
        // 3.25.0 — the generator would derive `Default` — but the fields are
        // written out anyway so that a provider version which promotes one
        // of them to required does not silently change the program's shape.
        let resource_group = resources::ResourceGroup::new(
            &ctx,
            "aks-rg",
            resources::ResourceGroupArgs {
                location: None,
                managed_by: None,
                resource_group_name: None,
                tags: None,
            },
            pulumi::ResourceOptions::default(),
        );

        // The cluster. `ManagedClusterArgs` requires `resourceGroupName`, so
        // the generator does not derive `Default` for it and Rust needs
        // every field named; the ones this program leaves alone are `None`.
        let cluster = containerservice::ManagedCluster::new(
            &ctx,
            "aks-cluster",
            containerservice::ManagedClusterArgs {
                resource_group_name: resource_group.name(),

                // A system-assigned managed identity is created and bound to
                // the cluster by Azure. This is what lets the example skip
                // the azuread provider entirely: with
                // `servicePrincipalProfile` you would first have to create
                // an application, a service principal and a password, then
                // pass the client id and secret in here.
                //
                // `ManagedClusterIdentityArgs` is all-optional, so it does
                // derive `Default` and the rest of its fields can be elided.
                identity: Some(types::ContainerserviceManagedClusterIdentityArgs {
                    r#type: Some(pulumi::pv::string("SystemAssigned").cast()),
                    ..Default::default()
                }),

                // One agent pool. `ManagedClusterAgentPoolProfileArgs` has a
                // required `name`, so it has no `Default` either and all
                // fifty of its fields appear below.
                agent_pool_profiles: Some(vec![
                    types::ContainerserviceManagedClusterAgentPoolProfileArgs {
                        // Pool names are lowercase alphanumeric, at most 12
                        // characters for a Linux pool.
                        name: pulumi::pv::string("agentpool").cast(),
                        count: Some(node_count.cast()),
                        vm_size: Some(node_vm_size.cast()),
                        // `System` pools host cluster-critical pods such as
                        // CoreDNS; every cluster needs exactly one.
                        mode: Some(pulumi::pv::string("System").cast()),
                        os_type: Some(pulumi::pv::string("Linux").cast()),
                        // Scale sets are the only backing Azure still
                        // recommends; availability sets are deprecated.
                        r#type: Some(pulumi::pv::string("VirtualMachineScaleSets").cast()),
                        max_pods: Some(pulumi::pv::number(110.0).cast()),
                        os_disk_size_gb: Some(pulumi::pv::number(30.0).cast()),

                        availability_zones: None,
                        capacity_reservation_group_id: None,
                        creation_data: None,
                        enable_auto_scaling: None,
                        enable_encryption_at_host: None,
                        enable_fips: None,
                        enable_node_public_ip: None,
                        enable_ultra_ssd: None,
                        gateway_profile: None,
                        gpu_instance_profile: None,
                        gpu_profile: None,
                        host_group_id: None,
                        kubelet_config: None,
                        kubelet_disk_type: None,
                        linux_osconfig: None,
                        local_dnsprofile: None,
                        max_count: None,
                        message_of_the_day: None,
                        min_count: None,
                        network_profile: None,
                        node_labels: None,
                        node_public_ipprefix_id: None,
                        node_taints: None,
                        orchestrator_version: None,
                        os_disk_type: None,
                        os_sku: None,
                        pod_ipallocation_mode: None,
                        pod_subnet_id: None,
                        power_state: None,
                        proximity_placement_group_id: None,
                        scale_down_mode: None,
                        scale_set_eviction_policy: None,
                        scale_set_priority: None,
                        security_profile: None,
                        spot_max_price: None,
                        tags: None,
                        upgrade_settings: None,
                        virtual_machine_nodes_status: None,
                        virtual_machines_profile: None,
                        vnet_subnet_id: None,
                        windows_profile: None,
                        workload_runtime: None,
                    },
                ]),

                // The label on the cluster's public API server hostname:
                // `<dnsPrefix>-<hash>.hcp.<region>.azmk8s.io`. Deriving it
                // from the resource group's own (auto-named, suffixed) name
                // keeps it unique per stack.
                dns_prefix: Some(resource_group.name().cast()),
                enable_rbac: Some(pulumi::pv::bool(true).cast()),

                aad_profile: None,
                addon_profiles: None,
                ai_toolchain_operator_profile: None,
                api_server_access_profile: None,
                auto_scaler_profile: None,
                auto_upgrade_profile: None,
                azure_monitor_profile: None,
                bootstrap_profile: None,
                disable_local_accounts: None,
                disk_encryption_set_id: None,
                extended_location: None,
                fqdn_subdomain: None,
                http_proxy_config: None,
                identity_profile: None,
                ingress_profile: None,
                kind: None,
                // Unset, so Azure picks its current default Kubernetes
                // version. Pin it here to control upgrades.
                kubernetes_version: None,
                // Unset, so the cluster gets no SSH login on its nodes. Set
                // it to a
                // `types::ContainerserviceContainerServiceLinuxProfileArgs`
                // — admin username plus a public key — to allow node SSH.
                linux_profile: None,
                location: None,
                metrics_profile: None,
                network_profile: None,
                node_provisioning_profile: None,
                // Unset, so Azure names the auto-created `MC_*` group that
                // holds the cluster's nodes, disks and load balancers.
                node_resource_group: None,
                node_resource_group_profile: None,
                oidc_issuer_profile: None,
                pod_identity_profile: None,
                private_link_resources: None,
                public_network_access: None,
                resource_name: None,
                security_profile: None,
                service_mesh_profile: None,
                // The alternative to `identity` above: a pre-created Entra
                // ID application's client id and secret.
                service_principal_profile: None,
                sku: None,
                storage_profile: None,
                support_plan: None,
                tags: None,
                upgrade_settings: None,
                windows_profile: None,
                workload_auto_scaler_profile: None,
            },
            pulumi::ResourceOptions::default(),
        );

        // ---------------------------------------------------------------
        // Fetching the kubeconfig.
        //
        // `listManagedClusterUserCredentials` is a provider *invoke*, which
        // the generator emits as a free function taking `(&ctx, args,
        // InvokeOptions)`. Its arguments are `Output`s, so passing the
        // cluster's own name is legal and makes this the output-versioned
        // form: the engine waits for the cluster before calling it, records
        // the dependency, and during a preview — when the name is still
        // unknown — skips the call and leaves the result unknown.
        //
        // `User` credentials are the non-admin ones; the admin equivalent is
        // `listManagedClusterAdminCredentials`.
        // ---------------------------------------------------------------
        let credentials = containerservice::list_managed_cluster_user_credentials(
            &ctx,
            containerservice::ListManagedClusterUserCredentialsArgs {
                resource_group_name: resource_group.name(),
                resource_name: cluster.name(),
                format: None,
                server_fqdn: None,
            },
            pulumi::InvokeOptions::default(),
        );

        // The invoke resolves to a typed result struct, so picking the
        // kubeconfig out is ordinary Rust inside `map`: `kubeconfigs` is a
        // `Vec` of credential records and Azure returns one, which is the
        // `kubeconfigs[0].value` the TypeScript and Go versions of this
        // example index. `map` does not run while the value is unknown, so
        // the indexing cannot panic during a preview.
        let encoded = credentials.map(
            |result: types::ContainerserviceListManagedClusterUserCredentialsResult| {
                result.kubeconfigs[0].value.clone()
            },
        );

        // The value is a base64-encoded YAML document. The core SDK ships
        // the decoder — `pulumi::pv::from_base64`, the same helper generated
        // programs use for PCL's `fromBase64` — so no `base64` crate
        // dependency is needed here.
        let kubeconfig = pulumi::pv::from_base64(encoded.cast());

        ctx.export(
            "resourceGroupName",
            resource_group.name().cast::<pulumi::PropertyValue>(),
        );
        ctx.export("clusterName", cluster.name().cast::<pulumi::PropertyValue>());

        // A kubeconfig carries a client certificate and key: whoever holds
        // it is an authenticated cluster user. Nothing in the schema marks
        // it sensitive, so the program marks it here — that encrypts it in
        // the state file and makes the CLI print `[secret]` for it unless
        // `pulumi stack output --show-secrets` is passed.
        ctx.export("kubeconfig", kubeconfig.as_secret());

        Ok(())
    });
}
