import {
    Box,
    Button,
    Center,
    Flex,
    Grid,
    HStack,
    Input,
    SimpleGrid,
    StackDivider,
    Text,
    VStack,
} from "@chakra-ui/react";
import React from "react";
import { Link as RouteLink, Route, Switch } from "react-router-dom";
import "./App.css";
import CFD from "./components/CFD";

function App() {
    return (
        <Center marginTop={50}>
            <HStack>
                <Box marginRight={5}>
                    <VStack align={"top"}>
                        <NavLink text={"trade"} path={"/trade"} />
                        <NavLink text={"wallet"} path={"/wallet"} />
                        <NavLink text={"settings"} path={"/settings"} />
                    </VStack>
                </Box>
                <Box width={1200} height={600}>
                    <Switch>
                        <Route path="/trade">
                            <Flex direction={"row"} height={"100%"}>
                                <Flex direction={"row"} width={"100%"}>
                                    <VStack
                                        spacing={5}
                                        shadow={"md"}
                                        padding={5}
                                        width={"100%"}
                                        divider={<StackDivider borderColor="gray.200" />}
                                    >
                                        <Box width={"100%"} overflow={"scroll"}>
                                            <SimpleGrid columns={2} spacing={10}>
                                                <CFD
                                                    number={1}
                                                    liquidation_price={42000}
                                                    amount={10000}
                                                    profit={200}
                                                    creation_date={new Date(Date.now(
                                                    ))}
                                                    status="ongoing"
                                                />
                                                <CFD
                                                    number={2}
                                                    liquidation_price={42000}
                                                    amount={12000}
                                                    profit={500}
                                                    creation_date={new Date(Date.now(
                                                    ))}
                                                    status="requested"
                                                />
                                            </SimpleGrid>
                                        </Box>
                                    </VStack>
                                </Flex>
                                <Flex width={"50%"} marginLeft={5}>
                                    <VStack spacing={5} shadow={"md"} padding={5} align={"stretch"}>
                                        <HStack>
                                            <Text align={"left"}>Your balance:</Text>
                                            <Text>10323</Text>
                                        </HStack>
                                        <HStack>
                                            <Text align={"left"}>Current Price:</Text>
                                            <Text>49000</Text>
                                        </HStack>
                                        <HStack>
                                            <Text>Quantity:</Text>
                                            <Input></Input>
                                        </HStack>
                                        <Text>Leverage:</Text>
                                        {/* TODO: consider button group */}
                                        <Flex justifyContent={"space-between"}>
                                            <Button disabled={true}>x1</Button>
                                            <Button disabled={true}>x2</Button>
                                            <Button colorScheme="blue" variant="solid">x5</Button>
                                        </Flex>
                                        <Button variant={"solid"} colorScheme={"blue"}>BUY</Button>
                                    </VStack>
                                </Flex>
                            </Flex>
                        </Route>
                        <Route path="/wallet">
                            <Center height={"100%"} shadow={"md"}>
                                <Box>
                                    <Text>Wallet</Text>
                                </Box>
                            </Center>
                        </Route>
                        <Route path="/settings">
                            <Center height={"100%"} shadow={"md"}>
                                <Box>
                                    <Text>Settings</Text>
                                </Box>
                            </Center>
                        </Route>
                    </Switch>
                </Box>
            </HStack>
        </Center>
    );
}

type NavLinkProps = { text: string; path: string };

const NavLink = ({ text, path }: NavLinkProps) => (
    <RouteLink to={path}>
        <Route
            path={path}
            children={({ match }) => (
                <Button width="100px" colorScheme="blue" variant={match?.path ? "solid" : "outline"}>
                    {text}
                </Button>
            )}
        />
    </RouteLink>
);

export default App;
