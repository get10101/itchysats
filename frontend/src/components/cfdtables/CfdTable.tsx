import {
    CheckCircleIcon,
    CheckIcon,
    ChevronRightIcon,
    ChevronUpIcon,
    CloseIcon,
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
    Table as CUITable,
    Tbody,
    Td,
    Th,
    Thead,
    Tr,
    useToast,
} from "@chakra-ui/react";
import React from "react";
import { useAsync } from "react-async";
import { Column, Row, useExpanded, useSortBy, useTable } from "react-table";
import { Action, Cfd } from "../Types";

interface CfdTableProps {
    data: Cfd[];
}

export function CfdTable(
    { data }: CfdTableProps,
) {
    const toast = useToast();

    let { run: postAction, isLoading: isActioning } = useAsync({
        deferFn: async ([orderId, action]: any[]) => {
            try {
                await doPostAction(orderId, action);
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
                accessor: "order_id", // accessor is the "key" in the data
            },
            {
                Header: "Position",
                accessor: ({ position }) => {
                    return (
                        <Badge colorScheme={position.getColorScheme()}>{position.key}</Badge>
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
                accessor: "profit_btc",
                isNumeric: true,
            },
            {
                Header: "Unrealized P/L %",
                accessor: "profit_in_percent",
                isNumeric: true,
            },
            {
                Header: "Timestamp",
                accessor: "state_transition_timestamp",
            },
            {
                Header: "State",
                accessor: ({ state }) => {
                    return (
                        <Badge colorScheme={state.getColorScheme()}>{state.getLabel()}</Badge>
                    );
                },
            },
            {
                Header: "Action",
                accessor: ({ actions, order_id }) => {
                    const actionIcons = actions.map((action) => {
                        return (<IconButton
                            key={action}
                            colorScheme={colorSchemaForAction(action)}
                            aria-label={action}
                            icon={iconForAction(action)}
                            onClick={() => postAction(order_id, action)}
                            isLoading={isActioning}
                        />);
                    });

                    return <HStack>{actionIcons}</HStack>;
                },
            },
        ],
        [isActioning, postAction],
    );

    // if we mark certain columns only as hidden, they are still around and we can render them in the sub-row
    const hiddenColumns = ["order_id", "leverage", "state_transition_timestamp"];

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
        case Action.ACCEPT_ROLL_OVER:
            return <CheckIcon />;
        case Action.REJECT_ROLL_OVER:
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
        case Action.ACCEPT_ROLL_OVER:
            return "green";
        case Action.REJECT_ROLL_OVER:
            return "red";
    }
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
                                    // @ts-ignore
                                    {...column.getHeaderProps(column.getSortByToggleProps())}
                                    // @ts-ignore
                                    isNumeric={column.isNumeric}
                                    textAlign={"right"}
                                >
                                    {column.render("Header")}
                                    <chakra.span>
                                        {// @ts-ignore
                                        column.isSorted
                                            ? (
                                                // @ts-ignore
                                                column.isSortedDesc
                                                    ? (
                                                        <TriangleDownIcon aria-label="sorted descending" />
                                                    )
                                                    : (
                                                        <TriangleUpIcon aria-label="sorted ascending" />
                                                    )
                                            )
                                            : null}
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

                                {// @ts-ignore
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
                                    : null}
                            </React.Fragment>
                        );
                    })}
                </Tbody>
            </CUITable>
        </>
    );
}

async function doPostAction(id: string, action: string) {
    await fetch(
        `/api/cfd/${id}/${action}`,
        { method: "POST", credentials: "include" },
    );
}
