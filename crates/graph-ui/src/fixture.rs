use super::{GraphSnapshot, WorkspaceRepositorySummary, WorkspaceSummary};
use beholder_dto::{
    EntityKind, EntityOrigin, EntityRef, EvidenceKind, EvidenceRef, QueryMetadata, RelationKind,
    SemanticEdge, TraversalMetadata,
};

pub const WORKSPACE_NAME: &str = "fresha-fixture";

const CHECKOUT: &str = "github.com/fresha/app-checkout";
const PACKAGES: &str = "github.com/fresha/app-packages";
const B2C: &str = "github.com/fresha/app-b2c-spa";
const CONTRACTS: &str = "github.com/fresha/proto-registry";

const CHECKOUT_MODULE: &str =
    "repo://github.com/fresha/app-checkout/elixir/module/CheckoutWeb.PackageResolver";
const CHECKOUT_RESOLVE: &str = "repo://github.com/fresha/app-checkout/elixir/function/CheckoutWeb.PackageResolver.resolve_package";
const CHECKOUT_TEST: &str = "repo://github.com/fresha/app-checkout/elixir/function/CheckoutWeb.PackageResolverTest.resolves_package";
const CHECKOUT_GRPC_CLIENT: &str = "repo://github.com/fresha/app-checkout/elixir/function/Fresha.Packages.V1.PackageService.Stub.create_pending_package_instances";
const CHECKOUT_OPERATION: &str = "graphql://checkoutPackage";
const PACKAGES_MODULE: &str =
    "repo://github.com/fresha/app-packages/elixir/module/Packages.PackageInstances";
const PACKAGES_CREATE: &str = "repo://github.com/fresha/app-packages/elixir/function/Packages.PackageInstances.create_pending";
const PACKAGES_PUBLISH: &str = "repo://github.com/fresha/app-packages/elixir/function/Packages.Events.publish_instance_created";
const SPA_MODULE: &str =
    "repo://github.com/fresha/app-b2c-spa/typescript/namespace/src/packages/api";
const SPA_CHECKOUT: &str =
    "repo://github.com/fresha/app-b2c-spa/typescript/callable/checkoutPackage";
const PROTO_FILE: &str = "proto://fresha/packages/v1/package_service.proto";
const PROTO_SERVICE: &str = "grpc://fresha.packages.v1.PackageService";
const CREATE_RPC: &str = "grpc://fresha.packages.v1.PackageService/CreatePendingPackageInstances";
const CREATE_REQUEST: &str = "proto://fresha.packages.v1.CreatePendingPackageInstancesRequest";
const CREATED_TOPIC: &str = "kafka://package-instance-created";
const STRIPE: &str = "external://stripe/PaymentIntent";

pub fn workspace() -> WorkspaceSummary {
    WorkspaceSummary {
        name: WORKSPACE_NAME.into(),
        repositories: [
            (CHECKOUT, "Checkout"),
            (PACKAGES, "Packages"),
            (B2C, "B2C web"),
            (CONTRACTS, "Proto registry"),
        ]
        .into_iter()
        .map(|(identity, display_name)| WorkspaceRepositorySummary {
            identity: identity.into(),
            display_name: display_name.into(),
        })
        .collect(),
    }
}

pub fn graph() -> GraphSnapshot {
    let nodes = vec![
        entity(
            CHECKOUT_MODULE,
            EntityKind::Namespace,
            "CheckoutWeb.PackageResolver",
            Some(CHECKOUT),
            EntityOrigin::Source,
            false,
        ),
        entity(
            CHECKOUT_RESOLVE,
            EntityKind::Callable,
            "resolve_package/3",
            Some(CHECKOUT),
            EntityOrigin::Source,
            false,
        ),
        entity(
            CHECKOUT_TEST,
            EntityKind::Callable,
            "resolves package",
            Some(CHECKOUT),
            EntityOrigin::Source,
            true,
        ),
        entity(
            CHECKOUT_GRPC_CLIENT,
            EntityKind::Callable,
            "PackageService.Stub.create_pending_package_instances/2",
            Some(CHECKOUT),
            EntityOrigin::Generated,
            false,
        ),
        entity(
            CHECKOUT_OPERATION,
            EntityKind::GraphqlOperation,
            "checkoutPackage",
            Some(CHECKOUT),
            EntityOrigin::Source,
            false,
        ),
        entity(
            PACKAGES_MODULE,
            EntityKind::Namespace,
            "Packages.PackageInstances",
            Some(PACKAGES),
            EntityOrigin::Source,
            false,
        ),
        entity(
            PACKAGES_CREATE,
            EntityKind::Callable,
            "create_pending/2",
            Some(PACKAGES),
            EntityOrigin::Source,
            false,
        ),
        entity(
            PACKAGES_PUBLISH,
            EntityKind::Callable,
            "publish_instance_created/1",
            Some(PACKAGES),
            EntityOrigin::Source,
            false,
        ),
        entity(
            SPA_MODULE,
            EntityKind::Namespace,
            "packages/api",
            Some(B2C),
            EntityOrigin::Source,
            false,
        ),
        entity(
            SPA_CHECKOUT,
            EntityKind::Callable,
            "checkoutPackage",
            Some(B2C),
            EntityOrigin::Source,
            false,
        ),
        entity(
            PROTO_FILE,
            EntityKind::ProtoFile,
            "package_service.proto",
            Some(CONTRACTS),
            EntityOrigin::Source,
            false,
        ),
        entity(
            PROTO_SERVICE,
            EntityKind::ProtoService,
            "PackageService",
            Some(CONTRACTS),
            EntityOrigin::Source,
            false,
        ),
        entity(
            CREATE_RPC,
            EntityKind::Rpc,
            "CreatePendingPackageInstances",
            Some(CONTRACTS),
            EntityOrigin::Source,
            false,
        ),
        entity(
            CREATE_REQUEST,
            EntityKind::ProtoMessage,
            "CreatePendingPackageInstancesRequest",
            Some(CONTRACTS),
            EntityOrigin::Source,
            false,
        ),
        entity(
            CREATED_TOPIC,
            EntityKind::KafkaTopic,
            "package-instance-created",
            Some(PACKAGES),
            EntityOrigin::Source,
            false,
        ),
        entity(
            STRIPE,
            EntityKind::Service,
            "Stripe PaymentIntent",
            None,
            EntityOrigin::ExternalDependency,
            false,
        ),
    ];
    let edges = vec![
        edge(
            "e01",
            CHECKOUT_MODULE,
            CHECKOUT_RESOLVE,
            RelationKind::Defines,
            1.0,
            Some(CHECKOUT),
            Some("lib/checkout_web/resolvers/package_resolver.ex"),
            Some(18),
        ),
        edge(
            "e02",
            CHECKOUT_MODULE,
            CHECKOUT_TEST,
            RelationKind::Defines,
            1.0,
            Some(CHECKOUT),
            Some("test/checkout_web/resolvers/package_resolver_test.exs"),
            Some(12),
        ),
        edge(
            "e03",
            CHECKOUT_TEST,
            CHECKOUT_RESOLVE,
            RelationKind::Calls,
            1.0,
            Some(CHECKOUT),
            Some("test/checkout_web/resolvers/package_resolver_test.exs"),
            Some(31),
        ),
        edge(
            "e04",
            CHECKOUT_RESOLVE,
            CHECKOUT_GRPC_CLIENT,
            RelationKind::Calls,
            1.0,
            Some(CHECKOUT),
            Some("lib/checkout_web/resolvers/package_resolver.ex"),
            Some(42),
        ),
        edge(
            "e04b",
            CHECKOUT_GRPC_CLIENT,
            CREATE_RPC,
            RelationKind::CallsRpc,
            1.0,
            Some(CHECKOUT),
            Some("lib/generated/fresha/packages/v1/package_service.pb.ex"),
            Some(88),
        ),
        edge(
            "e05",
            PACKAGES_MODULE,
            PACKAGES_CREATE,
            RelationKind::Defines,
            1.0,
            Some(PACKAGES),
            Some("lib/packages/package_instances.ex"),
            Some(26),
        ),
        edge(
            "e06",
            CREATE_RPC,
            PACKAGES_CREATE,
            RelationKind::ImplementedBy,
            1.0,
            Some(PACKAGES),
            Some("lib/packages/grpc/package_service.ex"),
            Some(15),
        ),
        edge(
            "e07",
            PACKAGES_CREATE,
            STRIPE,
            RelationKind::Requires,
            0.92,
            Some(PACKAGES),
            Some("lib/packages/package_instances.ex"),
            Some(63),
        ),
        edge(
            "e08",
            PACKAGES_CREATE,
            PACKAGES_PUBLISH,
            RelationKind::Calls,
            1.0,
            Some(PACKAGES),
            Some("lib/packages/package_instances.ex"),
            Some(79),
        ),
        edge(
            "e09",
            PACKAGES_PUBLISH,
            CREATED_TOPIC,
            RelationKind::Publishes,
            1.0,
            Some(PACKAGES),
            Some("lib/packages/events.ex"),
            Some(38),
        ),
        edge(
            "e10",
            CREATED_TOPIC,
            CHECKOUT_RESOLVE,
            RelationKind::ConsumedBy,
            0.85,
            Some(CHECKOUT),
            Some("lib/checkout/events/package_instance_created.ex"),
            Some(11),
        ),
        edge(
            "e11",
            SPA_MODULE,
            SPA_CHECKOUT,
            RelationKind::Defines,
            1.0,
            Some(B2C),
            Some("src/packages/api/checkout-package.ts"),
            Some(9),
        ),
        edge(
            "e12",
            SPA_CHECKOUT,
            CHECKOUT_OPERATION,
            RelationKind::CallsGraphql,
            0.96,
            Some(B2C),
            Some("src/packages/api/checkout-package.ts"),
            Some(28),
        ),
        edge(
            "e12b",
            CHECKOUT_OPERATION,
            CHECKOUT_RESOLVE,
            RelationKind::ResolvedBy,
            1.0,
            Some(CHECKOUT),
            Some("lib/checkout_web/schema/package_types.ex"),
            Some(54),
        ),
        edge(
            "e13",
            PROTO_FILE,
            PROTO_SERVICE,
            RelationKind::Defines,
            1.0,
            Some(CONTRACTS),
            Some("fresha/packages/v1/package_service.proto"),
            Some(7),
        ),
        edge(
            "e14",
            PROTO_SERVICE,
            CREATE_RPC,
            RelationKind::Defines,
            1.0,
            Some(CONTRACTS),
            Some("fresha/packages/v1/package_service.proto"),
            Some(11),
        ),
        edge(
            "e15",
            PROTO_FILE,
            CREATE_REQUEST,
            RelationKind::Defines,
            1.0,
            Some(CONTRACTS),
            Some("fresha/packages/v1/package_service.proto"),
            Some(17),
        ),
        edge(
            "e16",
            CREATE_RPC,
            CREATE_REQUEST,
            RelationKind::RequestType,
            1.0,
            Some(CONTRACTS),
            Some("fresha/packages/v1/package_service.proto"),
            Some(11),
        ),
    ];
    GraphSnapshot {
        schema: "beholder.graph-ui.fixture.v1",
        workspace: workspace(),
        metadata: QueryMetadata::completed("fixture:fresha", 1),
        traversal: TraversalMetadata {
            max_hops: 8,
            truncated: false,
        },
        nodes,
        edges,
    }
}

fn entity(
    id: &str,
    kind: EntityKind,
    name: &str,
    repository: Option<&str>,
    origin: EntityOrigin,
    test: bool,
) -> EntityRef {
    EntityRef {
        id: id.into(),
        kind,
        name: name.into(),
        repository: repository.map(Into::into),
        origin,
        test,
        metadata: None,
    }
}

#[allow(clippy::too_many_arguments)]
fn edge(
    id: &str,
    from: &str,
    to: &str,
    kind: RelationKind,
    confidence: f32,
    repository: Option<&str>,
    path: Option<&str>,
    line: Option<u32>,
) -> SemanticEdge {
    SemanticEdge {
        id: id.into(),
        from: from.into(),
        to: to.into(),
        kind,
        confidence,
        evidence: vec![EvidenceRef {
            source_kind: EvidenceKind::Ast,
            repository: repository.map(Into::into),
            path: path.map(Into::into),
            line,
            detail: None,
        }],
    }
}
