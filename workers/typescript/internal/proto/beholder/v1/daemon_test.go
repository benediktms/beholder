package v1

import (
	"testing"

	"google.golang.org/protobuf/proto"
)

func TestTraverseGraphBinding(t *testing.T) {
	input := &TraverseGraphRequest{
		Workspace: "main", Start: "root",
		Direction: GraphDirection_GRAPH_DIRECTION_DEPENDENCIES,
		MaxHops:   proto.Uint32(8), MaxPaths: proto.Uint32(50),
	}
	data, err := proto.Marshal(input)
	if err != nil {
		t.Fatal(err)
	}
	var decoded TraverseGraphRequest
	if err := proto.Unmarshal(data, &decoded); err != nil {
		t.Fatal(err)
	}
	if !proto.Equal(input, &decoded) {
		t.Fatalf("request changed during round-trip: %v", &decoded)
	}
	method := File_beholder_v1_daemon_proto.Services().ByName("Daemon").Methods().ByName("TraverseGraph")
	if method == nil || method.Input().FullName() != "beholder.v1.TraverseGraphRequest" || method.Output().FullName() != "beholder.v1.TraverseGraphResponse" {
		t.Fatalf("missing or incorrect TraverseGraph method: %v", method)
	}
}
