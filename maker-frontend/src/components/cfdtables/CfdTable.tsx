import {
    CheckCircleIcon,
    CheckIcon,
    ChevronRightIcon,
    ChevronUpIcon,
    CloseIcon,
    ExternalLinkIcon,
    RepeatIcon,
    TriangleDownIcon,
    TriangleUpIcon,
    WarningIcon,
} from "@chakra-ui/icons";
import {
    Badge,
    Box,
    chakra,
    HStack,
    IconButton,
    Link,
    Table as CUITable,
    Tbody,
    Td,
    Text,
    Th,
    Thead,
    Tooltip,
    Tr,
    useToast,
    VStack,
} from "@chakra-ui/react";
import { useAsync } from "@react-hookz/web";
import React from "react";
import { Column, Row, useExpanded, useSortBy, useTable } from "react-table";
import createErrorToast from "../ErrorToast";
import { HttpError } from "../HttpError";
import Timestamp from "../Timestamp";
import { Action, Cfd } from "../Types";

interface CfdTableProps {
    data: Cfd[];
}

export function CfdTable(
    { data }: CfdTableProps,
) {
    const toast = useToast();

    const [{ status: postActionStatus }, { execute: postAction }] = useAsync(
        async (orderId: string, action: string) => {
            try {
                await doPostAction(orderId, action);
            } catch (e) {
                createErrorToast(toast, e);
            }
        },
    );
    const isActioning = postActionStatus === "loading";

    const tableData = React.useMemo(
        () => data,
        [data],
    );

    const columns: Array<Column<Cfd>> = React.useMemo(
        () => [
            {
                id: "expander",
                Header: () => null,
                Cell: ({ row }: any) => (
                    <span>
                        {row.isExpanded
                            ? (
                                <IconButton
                                    aria-label="Reduce"
                                    icon={<ChevronUpIcon />}
                                    onClick={() => {
                                        row.toggleRowExpanded();
                                    }}
                                />
                            )
                            : (
                                <IconButton
                                    aria-label="Expand"
                                    icon={<ChevronRightIcon />}
                                    onClick={() => {
                                        row.toggleRowExpanded();
                                    }}
                                />
                            )}
                    </span>
                ),
            },
            {
                Header: "OrderId",
                accessor: "order_id", // accessor is the "key" in the data
            },
            {
                Header: "Details",
                accessor: ({ details, expiry_timestamp, order_id }) => {
                    const txs = details.tx_url_list.map((tx) => {
                        return (
                            <Link href={tx.url} key={tx.url} isExternal>
                                {tx.label + " transaction"}
                                <ExternalLinkIcon mx="2px" />
                            </Link>
                        );
                    });

                    return (
                        <Box>
                            <VStack>
                                <HStack>
                                    <Text>ID:</Text>
                                    <Text>{order_id}</Text>
                                </HStack>
                                {txs}
                                {expiry_timestamp && (
                                    <HStack>
                                        <Text>Expires on:</Text>
                                        <Timestamp timestamp={expiry_timestamp} />
                                    </HStack>
                                )}
                            </VStack>
                        </Box>
                    );
                },
            },
            {
                Header: "Position",
                accessor: ({ position }) => {
                    return <Badge colorScheme={position.getColorScheme()}>{position.key}</Badge>;
                },
                isNumeric: true,
            },

            {
                Header: "Quantity",
                accessor: ({ quantity_usd }) => {
                    return <Dollars amount={quantity_usd} />;
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
                    return <Dollars amount={initial_price} />;
                },
                isNumeric: true,
            },
            {
                Header: "Liquidation Price",
                isNumeric: true,
                accessor: ({ liquidation_price }) => {
                    return <Dollars amount={liquidation_price} />;
                },
            },
            {
                Header: "Closing Price",
                isNumeric: true,
                accessor: ({ closing_price }) => {
                    return <Dollars amount={closing_price || 0} />;
                },
            },
            {
                Header: "Unrealized P/L",
                accessor: "profit_btc",
                isNumeric: true,
            },
            {
                Header: "Unrealized P/L %",
                accessor: "profit_percent",
                isNumeric: true,
            },
            {
                Header: "Payout",
                accessor: "payout",
                isNumeric: true,
            },
            {
                Header: "State",
                accessor: ({ state }) => {
                    return <Badge colorScheme={state.getColorScheme()}>{state.getLabel()}</Badge>;
                },
            },
            {
                Header: "Action",
                accessor: ({ actions, order_id }) => {
                    const actionIcons = actions.map((action) => {
                        return (
                            <Tooltip label={action} key={action}>
                                <IconButton
                                    colorScheme={colorSchemaForAction(action)}
                                    aria-label={action}
                                    icon={iconForAction(action)}
                                    onClick={async () => await postAction(order_id, action)}
                                    isLoading={isActioning}
                                />
                            </Tooltip>
                        );
                    });

                    return <HStack>{actionIcons}</HStack>;
                },
            },
        ],
        [isActioning, postAction],
    );

    // if we mark certain columns only as hidden, they are still around and we can render them in the sub-row
    const hiddenColumns = ["order_id", "leverage", "Details"];

    return (
        <Table
            tableData={tableData}
            columns={columns}
            hiddenColumns={hiddenColumns}
            renderDetails={renderRowSubComponent}
        />
    );
}

function iconForAction(action: Action): any {
    switch (action) {
        case Action.ACCEPT_ORDER:
            return <CheckIcon />;
        case Action.REJECT_ORDER:
            return <CloseIcon />;
        case Action.COMMIT:
            return <WarningIcon />;
        case Action.SETTLE:
            return <CheckCircleIcon />;
        case Action.ACCEPT_SETTLEMENT:
            return <CheckIcon />;
        case Action.REJECT_SETTLEMENT:
            return <CloseIcon />;
        case Action.ROLL_OVER:
            return <RepeatIcon />;
        case Action.ACCEPT_ROLLOVER:
            return <CheckIcon />;
        case Action.REJECT_ROLLOVER:
            return <CloseIcon />;
    }
}

function colorSchemaForAction(action: Action): string {
    switch (action) {
        case Action.ACCEPT_ORDER:
            return "green";
        case Action.REJECT_ORDER:
            return "red";
        case Action.COMMIT:
            return "red";
        case Action.SETTLE:
            return "green";
        case Action.ROLL_OVER:
            return "green";
        case Action.ACCEPT_SETTLEMENT:
            return "green";
        case Action.REJECT_SETTLEMENT:
            return "red";
        case Action.ACCEPT_ROLLOVER:
            return "green";
        case Action.REJECT_ROLLOVER:
            return "red";
    }
}

function renderRowSubComponent(row: Row<Cfd>) {
    let cells = row.allCells
        .filter((cell) => {
            return ["Details", "Timestamp"].includes(cell.column.id);
        })
        .map((cell) => {
            return cell;
        });

    return (
        <>
            <HStack>
                {cells.map(cell => (
                    <Box key={cell.column.id}>
                        {cell.render("Cell")}
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

interface TableProps {
    columns: Array<Column<Cfd>>;
    tableData: Cfd[];
    hiddenColumns: string[];
    renderDetails: any;
}

export function Table({ columns, tableData, hiddenColumns, renderDetails }: TableProps) {
    const {
        getTableProps,
        getTableBodyProps,
        headerGroups,
        rows,
        prepareRow,
        visibleColumns,
    } = useTable(
        {
            columns,
            data: tableData,
            initialState: {
                hiddenColumns,
            },
            // @ts-ignore: this field exists and it works as expected.
            autoResetExpanded: false,
            autoResetSortBy: false,
        },
        useSortBy,
        useExpanded,
    );

    return (
        <>
            <CUITable {...getTableProps()} colorScheme="blue">
                <Thead>
                    {headerGroups.map((headerGroup) => (
                        <Tr {...headerGroup.getHeaderGroupProps()}>
                            {headerGroup.headers.map((column) => (
                                <Th
                                    {
                                        // @ts-ignore
                                        ...column.getHeaderProps(column.getSortByToggleProps())
                                    }
                                    // @ts-ignore
                                    isNumeric={column.isNumeric}
                                    textAlign={"right"}
                                >
                                    {column.render("Header")}
                                    <chakra.span>
                                        {
                                            // @ts-ignore
                                            column.isSorted
                                                ? (
                                                    // @ts-ignore
                                                    column.isSortedDesc
                                                        ? <TriangleDownIcon aria-label="sorted descending" />
                                                        : <TriangleUpIcon aria-label="sorted ascending" />
                                                )
                                                : null
                                        }
                                    </chakra.span>
                                </Th>
                            ))}
                        </Tr>
                    ))}
                </Thead>
                <Tbody {...getTableBodyProps()}>
                    {rows.map((row) => {
                        prepareRow(row);
                        return (
                            <React.Fragment key={row.id}>
                                <Tr
                                    {...row.getRowProps()}
                                >
                                    {row.cells.map((cell) => (
                                        // @ts-ignore
                                        <Td {...cell.getCellProps()} isNumeric={cell.column.isNumeric}>
                                            {cell.render("Cell")}
                                        </Td>
                                    ))}
                                </Tr>

                                {
                                    // @ts-ignore
                                    row.isExpanded
                                        ? (
                                            <Tr>
                                                <Td>
                                                </Td>
                                                <Td colSpan={visibleColumns.length - 1}>
                                                    {renderDetails(row)}
                                                </Td>
                                            </Tr>
                                        )
                                        : null
                                }
                            </React.Fragment>
                        );
                    })}
                </Tbody>
            </CUITable>
        </>
    );
}

async function doPostAction(id: string, action: string) {
    let res = await fetch(
        `/api/cfd/${id}/${action}`,
        { method: "POST", credentials: "include" },
    );
    if (!res.status.toString().startsWith("2")) {
        const resp = await res.json();
        throw new HttpError(resp);
    }
}
