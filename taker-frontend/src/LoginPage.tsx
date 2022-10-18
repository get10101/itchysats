import {
    Alert,
    AlertDescription,
    AlertIcon,
    Box,
    Button,
    Container,
    Flex,
    Heading,
    HStack,
    Input,
    InputGroup,
    InputRightElement,
    Stack,
    Text,
    useColorModeValue,
    VStack,
} from "@chakra-ui/react";
import * as React from "react";
import { FormEvent } from "react";
import { useState } from "react";
import useAuth from "./authentication/useAuth";
import logo from "./images/logo.svg";

export default function Login() {
    const { login, loading, error } = useAuth();
    const [show, setShow] = useState(false);
    const handleClick = () => setShow(!show);
    const [password, setPassword] = useState("");

    function handleSubmit(event: FormEvent<HTMLFormElement>) {
        event.preventDefault();
        login(password);
    }

    return (
        <Box>
            <form onSubmit={handleSubmit}>
                <Flex justifyContent={"center"} alignItems={"center"} height={"95vh"}>
                    <VStack spacing="6">
                        <Heading>Welcome Satoshi</Heading>
                        <img src={logo} className="Logo" alt="logo" />
                        <InputGroup size="md">
                            <Input
                                pr="4.5rem"
                                type={show ? "text" : "password"}
                                placeholder="Enter password"
                                value={password}
                                onChange={(e) => setPassword(e.target.value)}
                            />
                            <InputRightElement width="4.5rem">
                                <Button h="1.75rem" size="sm" onClick={handleClick}>
                                    {show ? "Hide" : "Show"}
                                </Button>
                            </InputRightElement>
                        </InputGroup>

                        <Button
                            disabled={loading}
                            variant={"solid"}
                            colorScheme={"blue"}
                            isLoading={loading}
                            type="submit"
                        >
                            Submit
                        </Button>

                        {error
                            && (
                                <Alert status="error">
                                    <AlertIcon />
                                    <AlertDescription>{error.detail}</AlertDescription>
                                </Alert>
                            )}
                    </VStack>
                </Flex>
            </form>
            <Box
                borderTopWidth={1}
                borderStyle={"solid"}
                borderColor={useColorModeValue("gray.200", "gray.700")}
            >
                <Container
                    as={Stack}
                    maxW={"6xl"}
                    py={4}
                    direction={{ base: "column", md: "row" }}
                    spacing={4}
                    justify={{ md: "center" }}
                    align={{ md: "center" }}
                >
                    <HStack>
                        <Text fontSize="xs">
                            The default password is&nbsp;
                            <Text fontSize="xs" as={"b"}>
                                "weareallsatoshi".
                            </Text>
                        </Text>
                    </HStack>
                </Container>
            </Box>
        </Box>
    );
}
