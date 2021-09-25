import { CheckIcon, ChevronRightIcon, ChevronUpIcon, CloseIcon } from "@chakra-ui/icons";
import { Badge, Box, HStack, IconButton, useToast } from "@chakra-ui/react";
import React from "react";
import { useAsync } from "react-async";
import { Column, Row } from "react-table";
import { postAcceptOrder, postRejectOrder } from "../../MakerClient";
import { Cfd } from "../Types";
import { Table } from "./CfdTable";

interface CfdMableTakerProps {
    data: Cfd[];
}

export function CfdTableMaker(
    { data }: CfdMableTakerProps,
) {
    const tableData = React.useMemo(
        () => data,
        [data],
    );

    const toast = useToast();

    let { run: acceptOrder, isLoading: isAccepting } = useAsync({
        deferFn: async ([orderId]: any[]) => {
            try {
                let payload = {
                    order_id: orderId,
                };
                await postAcceptOrder(payload);
            } catch (e) {
                const description = typeof e === "string" ? e : JSON.stringify(e);

                toast({
                    title: "Error",
                    description,
                    status: "error",
                    duration: 9000,
                    isClosable: true,
                });
            }
        },
    });

    let { run: rejectOrder, isLoading: isRejecting } = useAsync({
        deferFn: async ([orderId]: any[]) => {
            try {
                let payload = {
                    order_id: orderId,
                };
                await postRejectOrder(payload);
            } catch (e) {
                const description = typeof e === "string" ? e : JSON.stringify(e);

                toast({
                    title: "Error",
                    description,
                    status: "error",
                    duration: 9000,
                    isClosable: true,
                });
            }
        },
    });

    const columns: Array<Column<Cfd>> = React.useMemo(
        () => [
            {
                id: "expander",
                Header: () => null,
                Cell: ({ row }: any) => (
                    <span {...row.getToggleRowExpandedProps()}>
                        {row.isExpanded
                            ? <IconButton
                                aria-label="Reduce"
                                icon={<ChevronUpIcon />}
                                onClick={() => {
                                    row.toggleRowExpanded();
                                }}
                            />
                            : <IconButton
                                aria-label="Expand"
                                icon={<ChevronRightIcon />}
                                onClick={() => {
                                    row.toggleRowExpanded();
                                }}
                            />}
                    </span>
                ),
            },
            {
                Header: "OrderId",
                accessor: "order_id",
            },
            {
                Header: "Position",
                accessor: ({ position }) => {
                    let colorScheme = "green";
                    if (position.toLocaleLowerCase() === "buy") {
                        colorScheme = "purple";
                    }
                    return (
                        <Badge colorScheme={colorScheme}>{position}</Badge>
                    );
                },
                isNumeric: true,
            },

            {
                Header: "Quantity",
                accessor: ({ quantity_usd }) => {
                    return (<Dollars amount={quantity_usd} />);
                },
                isNumeric: true,
            },
            {
                Header: "Leverage",
                accessor: "leverage",
                isNumeric: true,
            },
            {
                Header: "Margin",
                accessor: "margin",
                isNumeric: true,
            },
            {
                Header: "Initial Price",
                accessor: ({ initial_price }) => {
                    return (<Dollars amount={initial_price} />);
                },
                isNumeric: true,
            },
            {
                Header: "Liquidation Price",
                isNumeric: true,
                accessor: ({ liquidation_price }) => {
                    return (<Dollars amount={liquidation_price} />);
                },
            },
            {
                Header: "Unrealized P/L",
                accessor: ({ profit_usd }) => {
                    return (<Dollars amount={profit_usd} />);
                },
                isNumeric: true,
            },
            {
                Header: "Timestamp",
                accessor: "state_transition_timestamp",
            },
            {
                Header: "Action",
                accessor: ({ state, order_id }) => {
                    if (state.toLowerCase() === "requested") {
                        return (<HStack>
                            <IconButton
                                colorScheme="green"
                                aria-label="Accept"
                                icon={<CheckIcon />}
                                onClick={async () => acceptOrder(order_id)}
                                isLoading={isAccepting}
                            />
                            <IconButton
                                colorScheme="red"
                                aria-label="Reject"
                                icon={<CloseIcon />}
                                onClick={async () => rejectOrder(order_id)}
                                isLoading={isRejecting}
                            />
                        </HStack>);
                    }

                    let colorScheme = "gray";
                    if (state.toLowerCase() === "rejected") {
                        colorScheme = "red";
                    }
                    if (state.toLowerCase() === "contract setup") {
                        colorScheme = "green";
                    }
                    return (
                        <Badge colorScheme={colorScheme}>{state}</Badge>
                    );
                },
            },
        ],
        [],
    );

    // if we mark certain columns only as hidden, they are still around and we can render them in the sub-row
    const hiddenColumns = ["order_id", "leverage", "Unrealized P/L", "state_transition_timestamp"];

    return (
        <Table
            tableData={tableData}
            columns={columns}
            hiddenColumns={hiddenColumns}
            renderDetails={renderRowSubComponent}
        />
    );
}

function renderRowSubComponent(row: Row<Cfd>) {
    // TODO: I would show additional information here such as txids, timestamps, actions
    let cells = row.allCells
        .filter((cell) => {
            return ["state_transition_timestamp"].includes(cell.column.id);
        })
        .map((cell) => {
            return cell;
        });

    return (
        <>
            Showing some more information here...
            <HStack>
                {cells.map(cell => (
                    <Box key={cell.column.id}>
                        {cell.column.id} = {cell.render("Cell")}
                    </Box>
                ))}
            </HStack>
        </>
    );
}

interface DollarsProps {
    amount: number;
}
function Dollars({ amount }: DollarsProps) {
    const price = Math.floor(amount * 100.0) / 100.0;
    return (
        <>
            $ {price}
        </>
    );
}
